use crate::audio::recording_preferences::get_default_recordings_folder;
use log::{error, info, warn};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Runtime};
use uuid::Uuid;

static PHONE_UPLOAD_SERVER: Lazy<Mutex<Option<PhoneUploadServer>>> = Lazy::new(|| Mutex::new(None));

#[derive(Debug)]
struct PhoneUploadServer {
    port: u16,
    token: String,
    upload_dir: PathBuf,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhoneUploadServerStatus {
    pub running: bool,
    pub port: Option<u16>,
    pub upload_url: Option<String>,
    pub local_url: Option<String>,
    pub upload_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhoneUploadReceived {
    pub path: String,
    pub filename: String,
    pub title: String,
    pub size_bytes: u64,
}

#[tauri::command]
pub async fn start_phone_upload_server_command<R: Runtime>(
    app: AppHandle<R>,
) -> Result<PhoneUploadServerStatus, String> {
    let mut guard = PHONE_UPLOAD_SERVER
        .lock()
        .map_err(|_| "Phone upload server lock poisoned".to_string())?;

    if let Some(server) = guard.as_ref() {
        return Ok(server_status(server));
    }

    let upload_dir = get_default_recordings_folder().join("phone-uploads");
    std::fs::create_dir_all(&upload_dir)
        .map_err(|e| format!("Failed to create phone upload directory: {}", e))?;

    let listener = TcpListener::bind("0.0.0.0:0")
        .map_err(|e| format!("Failed to start phone upload server: {}", e))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to read phone upload server port: {}", e))?
        .port();
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("Failed to configure phone upload server: {}", e))?;

    let token = Uuid::new_v4().to_string();
    let shutdown = Arc::new(AtomicBool::new(false));
    let thread_shutdown = shutdown.clone();
    let thread_upload_dir = upload_dir.clone();
    let thread_token = token.clone();

    let handle = thread::spawn(move || {
        info!("Phone upload server listening on 0.0.0.0:{}", port);
        while !thread_shutdown.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _addr)) => {
                    let app = app.clone();
                    let upload_dir = thread_upload_dir.clone();
                    let token = thread_token.clone();
                    thread::spawn(move || {
                        if let Err(e) = handle_connection(stream, app, upload_dir, token) {
                            warn!("Phone upload request failed: {}", e);
                        }
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    error!("Phone upload server accept failed: {}", e);
                    break;
                }
            }
        }
        info!("Phone upload server stopped");
    });

    let server = PhoneUploadServer {
        port,
        token,
        upload_dir,
        shutdown,
        handle: Some(handle),
    };
    let status = server_status(&server);
    *guard = Some(server);
    Ok(status)
}

#[tauri::command]
pub async fn stop_phone_upload_server_command() -> Result<(), String> {
    let mut guard = PHONE_UPLOAD_SERVER
        .lock()
        .map_err(|_| "Phone upload server lock poisoned".to_string())?;

    if let Some(mut server) = guard.take() {
        server.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", server.port));
        if let Some(handle) = server.handle.take() {
            let _ = handle.join();
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn get_phone_upload_server_status_command() -> Result<PhoneUploadServerStatus, String> {
    let guard = PHONE_UPLOAD_SERVER
        .lock()
        .map_err(|_| "Phone upload server lock poisoned".to_string())?;

    if let Some(server) = guard.as_ref() {
        Ok(server_status(server))
    } else {
        Ok(PhoneUploadServerStatus {
            running: false,
            port: None,
            upload_url: None,
            local_url: None,
            upload_dir: get_default_recordings_folder()
                .join("phone-uploads")
                .to_string_lossy()
                .to_string(),
        })
    }
}

fn server_status(server: &PhoneUploadServer) -> PhoneUploadServerStatus {
    let local_ip = local_network_ip().unwrap_or_else(|| "127.0.0.1".to_string());
    let upload_url = format!(
        "http://{}:{}/?token={}",
        local_ip, server.port, server.token
    );
    let local_url = format!("http://127.0.0.1:{}/?token={}", server.port, server.token);

    PhoneUploadServerStatus {
        running: true,
        port: Some(server.port),
        upload_url: Some(upload_url),
        local_url: Some(local_url),
        upload_dir: server.upload_dir.to_string_lossy().to_string(),
    }
}

fn handle_connection<R: Runtime>(
    mut stream: TcpStream,
    app: AppHandle<R>,
    upload_dir: PathBuf,
    token: String,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;

    let request = read_request_head(&mut stream)?;
    let token_ok = query_params(&request.path)
        .get("token")
        .map(|provided| provided == &token)
        .unwrap_or(false);

    if !token_ok {
        return write_response(&mut stream, 403, "text/plain", b"Forbidden");
    }

    match (
        request.method.as_str(),
        request.path_without_query().as_str(),
    ) {
        ("GET", "/") => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            upload_page(&token).as_bytes(),
        ),
        ("GET", "/health") => {
            write_response(&mut stream, 200, "application/json", br#"{"ok":true}"#)
        }
        ("POST", "/upload") => handle_upload(stream, request, app, upload_dir),
        _ => write_response(&mut stream, 404, "text/plain", b"Not found"),
    }
}

fn handle_upload<R: Runtime>(
    mut stream: TcpStream,
    request: HttpRequestHead,
    app: AppHandle<R>,
    upload_dir: PathBuf,
) -> Result<(), String> {
    let content_length = request
        .headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| "Missing Content-Length".to_string())?;

    if content_length == 0 {
        return write_response(&mut stream, 400, "text/plain", b"Empty upload");
    }

    let params = query_params(&request.path);
    let original_filename = params
        .get("filename")
        .map(|value| sanitize_filename(value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "voice-memo.m4a".to_string());
    let title = params
        .get("title")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| filename_stem(&original_filename));

    std::fs::create_dir_all(&upload_dir).map_err(|e| e.to_string())?;
    let stored_filename = unique_upload_filename(&original_filename);
    let destination = upload_dir.join(&stored_filename);
    let mut file = std::fs::File::create(&destination).map_err(|e| e.to_string())?;

    let mut written = 0usize;
    if !request.body_start.is_empty() {
        let bytes = request.body_start.len().min(content_length);
        file.write_all(&request.body_start[..bytes])
            .map_err(|e| e.to_string())?;
        written += bytes;
    }

    let mut buffer = [0u8; 64 * 1024];
    while written < content_length {
        let remaining = content_length - written;
        let read_len = buffer.len().min(remaining);
        let n = stream
            .read(&mut buffer[..read_len])
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("Upload ended before Content-Length bytes were received".to_string());
        }
        file.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        written += n;
    }

    let payload = PhoneUploadReceived {
        path: destination.to_string_lossy().to_string(),
        filename: original_filename,
        title,
        size_bytes: written as u64,
    };

    if let Err(e) = app.emit("phone-upload-received", payload.clone()) {
        warn!("Failed to emit phone-upload-received: {}", e);
    }

    let response = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;
    write_response(&mut stream, 200, "application/json", &response)
}

struct HttpRequestHead {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body_start: Vec<u8>,
}

impl HttpRequestHead {
    fn path_without_query(&self) -> String {
        self.path
            .split_once('?')
            .map(|(path, _)| path.to_string())
            .unwrap_or_else(|| self.path.clone())
    }
}

fn read_request_head(stream: &mut TcpStream) -> Result<HttpRequestHead, String> {
    let mut buffer = Vec::with_capacity(8192);
    let mut chunk = [0u8; 4096];
    let header_end;

    loop {
        let n = stream.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("Connection closed before request headers were complete".to_string());
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_header_end(&buffer) {
            header_end = pos;
            break;
        }
        if buffer.len() > 128 * 1024 {
            return Err("Request headers too large".to_string());
        }
    }

    let header_bytes = &buffer[..header_end];
    let body_start = buffer[header_end + 4..].to_vec();
    let header_text = String::from_utf8_lossy(header_bytes);
    let mut lines = header_text.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| "Missing HTTP request line".to_string())?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| "Missing HTTP method".to_string())?
        .to_string();
    let path = request_parts
        .next()
        .ok_or_else(|| "Missing HTTP path".to_string())?
        .to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    Ok(HttpRequestHead {
        method,
        path,
        headers,
        body_start,
    })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<(), String> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "OK",
    };
    let headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        reason,
        content_type,
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .and_then(|_| stream.write_all(body))
        .map_err(|e| e.to_string())
}

fn query_params(path: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    let Some((_, query)) = path.split_once('?') else {
        return params;
    };

    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(percent_decode(key), percent_decode(value));
    }

    params
}

fn percent_decode(value: &str) -> String {
    let mut bytes = Vec::with_capacity(value.len());
    let mut chars = value.as_bytes().iter().copied();

    while let Some(byte) = chars.next() {
        match byte {
            b'+' => bytes.push(b' '),
            b'%' => {
                let hi = chars.next();
                let lo = chars.next();
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    if let Ok(decoded) = u8::from_str_radix(&String::from_utf8_lossy(&[hi, lo]), 16)
                    {
                        bytes.push(decoded);
                    }
                }
            }
            other => bytes.push(other),
        }
    }

    String::from_utf8_lossy(&bytes).to_string()
}

fn sanitize_filename(filename: &str) -> String {
    filename
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ' ') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches([' ', '.'])
        .to_string()
}

fn unique_upload_filename(original: &str) -> String {
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    format!("phone_{}_{}", timestamp, original)
}

fn filename_stem(filename: &str) -> String {
    PathBuf::from(filename)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Phone Upload")
        .to_string()
}

fn local_network_ip() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

fn upload_page(token: &str) -> String {
    format!(
        r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Meetily Phone Upload</title>
  <style>
    body {{ font-family: -apple-system, BlinkMacSystemFont, sans-serif; margin: 24px; line-height: 1.4; }}
    label {{ display: block; margin: 16px 0 6px; font-weight: 600; }}
    input, button {{ box-sizing: border-box; width: 100%; font: inherit; padding: 12px; }}
    button {{ margin-top: 16px; }}
    #status {{ margin-top: 16px; }}
  </style>
</head>
<body>
  <h1>Meetily Upload</h1>
  <label for="title">Meeting title</label>
  <input id="title" placeholder="Voice memo" />
  <label for="file">Audio file</label>
  <input id="file" type="file" accept="audio/*,.m4a,.mp3,.wav,.aac,.flac,.ogg,.webm" />
  <button id="upload">Upload to Meetily</button>
  <div id="status"></div>
  <script>
    const token = "{}";
    const statusEl = document.getElementById("status");
    document.getElementById("upload").addEventListener("click", async () => {{
      const file = document.getElementById("file").files[0];
      if (!file) {{
        statusEl.textContent = "Choose an audio file first.";
        return;
      }}
      const title = document.getElementById("title").value || file.name;
      statusEl.textContent = "Uploading...";
      const params = new URLSearchParams({{ token, filename: file.name, title }});
      const response = await fetch(`/upload?${{params.toString()}}`, {{
        method: "POST",
        headers: {{ "Content-Type": "application/octet-stream" }},
        body: file,
      }});
      if (!response.ok) {{
        statusEl.textContent = `Upload failed: ${{response.status}}`;
        return;
      }}
      statusEl.textContent = "Uploaded. You can return to Meetily.";
    }});
  </script>
</body>
</html>"#,
        token
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_query_params() {
        let params = query_params("/upload?filename=Voice+Memo.m4a&title=Hello%20World");

        assert_eq!(params.get("filename"), Some(&"Voice Memo.m4a".to_string()));
        assert_eq!(params.get("title"), Some(&"Hello World".to_string()));
    }

    #[test]
    fn sanitizes_filename() {
        assert_eq!(sanitize_filename("../Voice:Memo?.m4a"), "_Voice_Memo_.m4a");
    }
}

use crate::api::TranscriptSegment;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct ExternalAsrResponse {
    #[serde(default)]
    pub segments: Vec<ExternalAsrSegment>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct ExternalAsrSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default, alias = "speaker", alias = "speaker_label")]
    pub speaker_label: Option<String>,
}

pub fn transcript_segments_from_external_asr(
    response: ExternalAsrResponse,
    timestamp: String,
) -> Vec<TranscriptSegment> {
    response
        .segments
        .into_iter()
        .filter_map(|segment| {
            let text = segment.text.trim();
            if text.is_empty() {
                return None;
            }

            Some(TranscriptSegment {
                id: format!("transcript-{}", Uuid::new_v4()),
                text: text.to_string(),
                timestamp: timestamp.clone(),
                speaker_label: segment
                    .speaker_label
                    .map(|speaker| speaker.trim().to_string())
                    .filter(|speaker| !speaker.is_empty()),
                audio_start_time: Some(segment.start),
                audio_end_time: Some(segment.end),
                duration: Some((segment.end - segment.start).max(0.0)),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_speakr_style_speaker_segments() {
        let payload = r#"{
            "segments": [
                {"start": 0.0, "end": 1.2, "text": " Thank you. ", "speaker": "SPEAKER_00"},
                {"start": 1.2, "end": 2.5, "text": "Yes, agreed.", "speaker": "SPEAKER_01"}
            ],
            "language": "en",
            "duration": 2.5
        }"#;

        let response: ExternalAsrResponse = serde_json::from_str(payload).unwrap();
        let segments =
            transcript_segments_from_external_asr(response, "2026-07-07T12:00:00Z".to_string());

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].text, "Thank you.");
        assert_eq!(segments[0].speaker_label.as_deref(), Some("SPEAKER_00"));
        assert_eq!(segments[0].audio_start_time, Some(0.0));
        assert_eq!(segments[0].audio_end_time, Some(1.2));
    }

    #[test]
    fn skips_blank_segments_and_clamps_negative_duration() {
        let response = ExternalAsrResponse {
            segments: vec![
                ExternalAsrSegment {
                    start: 3.0,
                    end: 2.0,
                    text: "Kept".to_string(),
                    speaker_label: Some(" ".to_string()),
                },
                ExternalAsrSegment {
                    start: 4.0,
                    end: 5.0,
                    text: " ".to_string(),
                    speaker_label: Some("SPEAKER_00".to_string()),
                },
            ],
            language: None,
            duration: None,
        };

        let segments = transcript_segments_from_external_asr(response, "now".to_string());

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].speaker_label, None);
        assert_eq!(segments[0].duration, Some(0.0));
    }
}

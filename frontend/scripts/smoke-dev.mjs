#!/usr/bin/env node

const DEFAULT_URL = 'http://localhost:3118';
const baseUrl = process.env.MEETILY_SMOKE_URL || DEFAULT_URL;

async function fetchText(url) {
  const response = await fetch(url);
  const text = await response.text();
  return { response, text };
}

function toAbsoluteUrl(src) {
  return new URL(src, baseUrl).toString();
}

const { response, text } = await fetchText(baseUrl);

if (!response.ok) {
  throw new Error(`Home page failed: ${response.status} ${response.statusText}`);
}

if (!text.includes('Welcome to meetily')) {
  throw new Error('Home page did not include the expected welcome text');
}

const scriptSrcs = [...text.matchAll(/<script[^>]+src="([^"]+)"/g)].map((match) => match[1]);

if (scriptSrcs.length === 0) {
  throw new Error('No script chunks found on the home page');
}

for (const src of scriptSrcs) {
  const scriptUrl = toAbsoluteUrl(src);
  const scriptResponse = await fetch(scriptUrl);
  const bytes = (await scriptResponse.arrayBuffer()).byteLength;

  if (!scriptResponse.ok) {
    throw new Error(`Script chunk failed: ${scriptResponse.status} ${src}`);
  }

  if (bytes === 0) {
    throw new Error(`Script chunk was empty: ${src}`);
  }

  console.log(`${scriptResponse.status} ${bytes} bytes ${src}`);
}

console.log(`Smoke passed: ${baseUrl}`);

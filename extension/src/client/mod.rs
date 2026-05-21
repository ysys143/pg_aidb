// HTTP client module — reqwest blocking (HANDOFF.md §3)
// 3-phase pattern: Phase 1 (SPI, inside tx) → Phase 2 (HTTP, outside SPI) → Phase 3 (SPI)
// This module is Phase 2 only: no SPI calls here.

use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct EmbeddingRequest<'a> {
    pub input: &'a str,
    pub model: &'a str,
}

#[derive(Deserialize)]
pub struct EmbeddingResponse {
    pub data: Vec<EmbeddingDatum>,
}

#[derive(Deserialize)]
pub struct EmbeddingDatum {
    pub embedding: Vec<f32>,
}

// ----------------------------------------------------------------
// Search / Ask types (RAG service, HANDOFF.md §3 Phase 2)
// ----------------------------------------------------------------

#[derive(Serialize)]
struct SearchRequest<'a> {
    query: &'a str,
    collection: &'a str,
    top_k: i32,
    filter: &'a serde_json::Value,
}

#[derive(Deserialize)]
pub struct ChunkResult {
    pub chunk_id: String,
    pub content: String,
    pub similarity: f32,
    pub source: String,
    pub metadata: serde_json::Value,
}

#[derive(Serialize)]
struct SearchMmrRequest<'a> {
    query: &'a str,
    collection: &'a str,
    top_k: i32,
    fetch_k: i32,
    lambda_param: f32,
    filter: &'a serde_json::Value,
}

#[derive(Serialize)]
struct AskRequest<'a> {
    query: &'a str,
    collection: &'a str,
    top_k: i32,
}

#[derive(Deserialize)]
struct AskResponse {
    answer: String,
}

#[derive(Deserialize)]
struct SearchResponse {
    results: Vec<ChunkResult>,
    // usage is logged on the rag side; not propagated to sync callers.
    #[serde(default)]
    #[allow(dead_code)]
    usage: serde_json::Value,
}

pub fn call_search(
    base_url: &str,
    api_key: Option<&str>,
    query: &str,
    collection: &str,
    top_k: i32,
    filter: &serde_json::Value,
) -> Result<Vec<ChunkResult>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("client build: {e}"))?;

    let mut req = client
        .post(format!("{base_url}/search"))
        .json(&SearchRequest { query, collection, top_k, filter });

    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }

    let resp: SearchResponse = req
        .send()
        .map_err(|e| format!("http send: {e}"))?
        .error_for_status()
        .map_err(|e| format!("http status: {e}"))?
        .json()
        .map_err(|e| format!("json decode: {e}"))?;

    Ok(resp.results)
}

pub fn call_search_mmr(
    base_url: &str,
    api_key: Option<&str>,
    query: &str,
    collection: &str,
    top_k: i32,
    fetch_k: i32,
    lambda_param: f32,
    filter: &serde_json::Value,
) -> Result<Vec<ChunkResult>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("client build: {e}"))?;

    let mut req = client
        .post(format!("{base_url}/search_mmr"))
        .json(&SearchMmrRequest { query, collection, top_k, fetch_k, lambda_param, filter });

    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }

    let resp: SearchResponse = req
        .send()
        .map_err(|e| format!("http send: {e}"))?
        .error_for_status()
        .map_err(|e| format!("http status: {e}"))?
        .json()
        .map_err(|e| format!("json decode: {e}"))?;

    Ok(resp.results)
}

pub fn call_ask(
    base_url: &str,
    api_key: Option<&str>,
    query: &str,
    collection: &str,
    top_k: i32,
) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("client build: {e}"))?;

    let mut req = client
        .post(format!("{base_url}/ask"))
        .json(&AskRequest { query, collection, top_k });

    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }

    let resp: AskResponse = req
        .send()
        .map_err(|e| format!("http send: {e}"))?
        .error_for_status()
        .map_err(|e| format!("http status: {e}"))?
        .json()
        .map_err(|e| format!("json decode: {e}"))?;

    Ok(resp.answer)
}

pub fn call_embeddings(
    base_url: &str,
    api_key: Option<&str>,
    input: &str,
    model: &str,
) -> Result<Vec<f32>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("client build: {e}"))?;

    let mut req = client
        .post(format!("{base_url}/v1/embeddings"))
        .json(&EmbeddingRequest { input, model });

    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }

    let resp: EmbeddingResponse = req
        .send()
        .map_err(|e| format!("http send: {e}"))?
        .error_for_status()
        .map_err(|e| format!("http status: {e}"))?
        .json()
        .map_err(|e| format!("json decode: {e}"))?;

    resp.data
        .into_iter()
        .next()
        .map(|d| d.embedding)
        .ok_or_else(|| "empty embedding data array".to_string())
}

use crate::handlers::{BoxFuture, Executor, ExecutorBody, ExecutorRequest, ExecutorResponse};

/// Bridges the chat pipeline to the upstream HTTP transport.
pub struct HttpExecutor(pub reqwest::Client);

impl Executor for HttpExecutor {
    fn execute<'a>(
        &'a self,
        req: ExecutorRequest,
    ) -> BoxFuture<'a, Result<ExecutorResponse, String>> {
        Box::pin(async move {
            let bedrock_xai = req.provider == "bedrock-xai";
            let vertex = matches!(req.provider.as_str(), "vertex" | "vertex-partner");
            let bedrock_stream = (req.provider == "bedrock" || bedrock_xai) && req.stream;
            let commandcode_stream = req.provider == "commandcode" && req.stream;
            let trae_provider = req.provider == "trae";
            let windsurf_provider = req.provider == "windsurf";
            let response_format = (req.provider == "github"
                && req.model.to_ascii_lowercase().contains("claude"))
            .then(|| "claude".to_string())
            .or_else(|| {
                (commandcode_stream || trae_provider || windsurf_provider)
                    .then(|| "openai".to_string())
            });
            let base_url = req
                .credentials
                .provider_specific_data
                .get("baseUrl")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            let model = req.model.clone();
            let response = crate::executors::execute(
                &self.0,
                crate::executors::ExecutorRequest {
                    provider: req.provider,
                    model: req.model,
                    body: req.body,
                    stream: req.stream,
                    api_key: req.credentials.api_key,
                    access_token: req.credentials.access_token,
                    base_url,
                    provider_specific_data: Some(req.credentials.provider_specific_data),
                },
            )
            .await
            .map_err(|e| e.to_string())?;
            let status = response.status().as_u16();
            let mut response_url = response.url().clone();
            if vertex && response_url.query().is_some() {
                let pairs: Vec<(String, String)> = response_url
                    .query_pairs()
                    .filter(|(name, _)| name != "key")
                    .map(|(name, value)| (name.into_owned(), value.into_owned()))
                    .collect();
                response_url.set_query(None);
                if !pairs.is_empty() {
                    response_url.query_pairs_mut().extend_pairs(pairs);
                }
            }
            let url = Some(response_url.to_string());
            let headers = response
                .headers()
                .iter()
                .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_string())))
                .collect();
            let bedrock_stream = bedrock_stream && (200..300).contains(&status);
            let commandcode_stream = commandcode_stream && (200..300).contains(&status);
            let trae_ok = trae_provider && (200..300).contains(&status);
            let windsurf_ok = windsurf_provider && (200..300).contains(&status);
            let body = if req.stream {
                let (tx, rx) = tokio::sync::mpsc::channel(16);
                tokio::spawn(async move {
                    let mut response = response;
                    let mut decoder = bedrock_stream
                        .then(|| crate::executors::bedrock::Decoder::new(bedrock_xai));
                    let mut cc_decoder = commandcode_stream
                        .then(|| crate::executors::commandcode::Decoder::new(&model));
                    let mut trae_decoder = trae_ok
                        .then(|| crate::executors::trae::Decoder::new(&model));
                    let mut ws_decoder = windsurf_ok
                        .then(|| crate::executors::windsurf::Decoder::new(&model));
                    loop {
                        match response.chunk().await {
                            Ok(Some(bytes)) => {
                                let bytes = if let Some(decoder) = decoder.as_mut() {
                                    match decoder.push(&bytes) {
                                        Ok(bytes) => bytes,
                                        Err(error) => {
                                            let _ = tx.send(Err(error.to_string())).await;
                                            break;
                                        }
                                    }
                                } else if let Some(decoder) = cc_decoder.as_mut() {
                                    match decoder.push(&bytes) {
                                        Ok(bytes) => bytes,
                                        Err(error) => {
                                            let _ = tx.send(Err(error.to_string())).await;
                                            break;
                                        }
                                    }
                                } else if let Some(decoder) = trae_decoder.as_mut() {
                                    match decoder.push(&bytes) {
                                        Ok(bytes) => bytes,
                                        Err(error) => {
                                            let _ = tx.send(Err(error.to_string())).await;
                                            break;
                                        }
                                    }
                                } else if let Some(decoder) = ws_decoder.as_mut() {
                                    match decoder.push(&bytes) {
                                        Ok(bytes) => bytes,
                                        Err(error) => {
                                            let _ = tx.send(Err(error.to_string())).await;
                                            break;
                                        }
                                    }
                                } else {
                                    bytes.to_vec()
                                };
                                if !bytes.is_empty() && tx.send(Ok(bytes)).await.is_err() {
                                    break;
                                }
                            }
                            Ok(None) => {
                                if let Some(decoder) = decoder.as_ref() {
                                    if let Err(error) = decoder.finish() {
                                        let _ = tx.send(Err(error.to_string())).await;
                                    } else if bedrock_xai {
                                        let _ = tx.send(Ok(b"data: [DONE]\n\n".to_vec())).await;
                                    }
                                } else if let Some(decoder) = cc_decoder.as_mut() {
                                    let tail = decoder.finish();
                                    if !tail.is_empty() {
                                        let _ = tx.send(Ok(tail)).await;
                                    }
                                } else if let Some(decoder) = trae_decoder.as_mut() {
                                    let tail = decoder.finish();
                                    if !tail.is_empty() {
                                        let _ = tx.send(Ok(tail)).await;
                                    }
                                } else if let Some(decoder) = ws_decoder.as_mut() {
                                    let tail = decoder.finish();
                                    if !tail.is_empty() {
                                        let _ = tx.send(Ok(tail)).await;
                                    }
                                }
                                break;
                            }
                            Err(e) => {
                                let _ = tx.send(Err(e.to_string())).await;
                                break;
                            }
                        }
                    }
                });
                ExecutorBody::Stream(rx)
            } else {
                let raw = response.bytes().await.map_err(|e| e.to_string())?.to_vec();
                if trae_ok {
                    ExecutorBody::Bytes(crate::executors::trae::decode_non_stream(&raw, &model))
                } else if windsurf_ok {
                    ExecutorBody::Bytes(crate::executors::windsurf::decode_non_stream(&raw, &model))
                } else {
                    ExecutorBody::Bytes(raw)
                }
            };
            Ok(ExecutorResponse {
                status,
                headers,
                body,
                url,
                response_format,
            })
        })
    }
}

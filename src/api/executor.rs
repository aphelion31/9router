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
            let bedrock_stream = (req.provider == "bedrock" || bedrock_xai) && req.stream;
            let response_format = (req.provider == "github"
                && req.model.to_ascii_lowercase().contains("claude"))
            .then(|| "claude".to_string());
            let base_url = req
                .credentials
                .provider_specific_data
                .get("baseUrl")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
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
                },
            )
            .await
            .map_err(|e| e.to_string())?;
            let status = response.status().as_u16();
            let url = Some(response.url().to_string());
            let headers = response
                .headers()
                .iter()
                .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_string())))
                .collect();
            let bedrock_stream = bedrock_stream && (200..300).contains(&status);
            let body = if req.stream {
                let (tx, rx) = tokio::sync::mpsc::channel(16);
                tokio::spawn(async move {
                    let mut response = response;
                    let mut decoder = bedrock_stream
                        .then(|| crate::executors::bedrock::Decoder::new(bedrock_xai));
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
                ExecutorBody::Bytes(response.bytes().await.map_err(|e| e.to_string())?.to_vec())
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

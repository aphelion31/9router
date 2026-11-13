use crate::handlers::{BoxFuture, Executor, ExecutorBody, ExecutorRequest, ExecutorResponse};

/// Bridges the chat pipeline to the upstream HTTP transport.
pub struct HttpExecutor(pub reqwest::Client);

impl Executor for HttpExecutor {
    fn execute<'a>(
        &'a self,
        req: ExecutorRequest,
    ) -> BoxFuture<'a, Result<ExecutorResponse, String>> {
        Box::pin(async move {
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
            let body = if req.stream {
                let (tx, rx) = tokio::sync::mpsc::channel(16);
                tokio::spawn(async move {
                    let mut response = response;
                    loop {
                        match response.chunk().await {
                            Ok(Some(bytes)) => {
                                if tx.send(Ok(bytes.to_vec())).await.is_err() {
                                    break;
                                }
                            }
                            Ok(None) => break,
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
                response_format: None,
            })
        })
    }
}

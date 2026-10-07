use std::ops::Deref;
#[derive(Clone, Debug)]
pub struct TestWrapper {
    inner: std::sync::Arc<Inner>,
}
#[derive(Debug)]
struct Inner {
    connection_provider: Box<
        dyn ::tonic_client_wrapper::ConnectionProvider<TestInnerClient>,
    >,
    connection: ::tokio::sync::Mutex<Option<InnerConnection>>,
}
#[derive(Debug)]
struct InnerConnection {
    client: TestInnerClient,
    created: std::time::SystemTime,
}
impl TestWrapper {
    pub fn build<P: ::tonic_client_wrapper::ConnectionProvider<TestInnerClient>>(
        connection_provider: P,
    ) -> Self {
        let inner = Inner {
            connection_provider: Box::new(connection_provider),
            connection: tokio::sync::Mutex::new(None),
        };
        Self {
            inner: std::sync::Arc::new(inner),
        }
    }
    pub async fn connection(
        &self,
    ) -> std::result::Result<TestInnerClient, tonic::Status> {
        let mut guard = self.inner.connection.lock().await;
        if let Some(connection) = guard.deref() {
            if self
                .inner
                .connection_provider
                .connection_is_stale(connection.created)
                .await
            {
                guard.take();
            }
        }
        match guard.deref() {
            Some(connection) => Ok(connection.client.clone()),
            None => {
                let client = self.inner.connection_provider.provide_connection().await?;
                guard
                    .replace(InnerConnection {
                        client: client.clone(),
                        created: std::time::SystemTime::now(),
                    });
                Ok(client)
            }
        }
    }
    pub fn url(&self) -> &str {
        self.inner.connection_provider.connection_url()
    }
    pub async fn golden_rpc(
        &self,
    ) -> Result<crate::test::GoldenResponse, tonic::Status> {
        ::carbide_instrument::red::instrumented(
                "golden_service",
                "golden_rpc",
                async move {
                    Ok(
                        self
                            .connection()
                            .await?
                            .golden_rpc(
                                tonic::Request::new(crate::test::GoldenRequest {}),
                            )
                            .await?
                            .into_inner(),
                    )
                },
            )
            .await
    }
}

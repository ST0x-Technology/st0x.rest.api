use base64::Engine;
use rocket::local::asynchronous::Client;

pub(crate) async fn client() -> Client {
    TestClientBuilder::new().build().await
}

pub(crate) struct TestClientBuilder {
    rate_limiter: crate::fairings::RateLimiter,
    token_list_url: Option<String>,
    raindex_registry_url: Option<String>,
    raindex_config: Option<crate::raindex::RaindexProvider>,
}

impl TestClientBuilder {
    pub(crate) fn new() -> Self {
        Self {
            rate_limiter: crate::fairings::RateLimiter::new(10000, 10000),
            token_list_url: None,
            raindex_registry_url: None,
            raindex_config: None,
        }
    }

    pub(crate) fn rate_limiter(mut self, rate_limiter: crate::fairings::RateLimiter) -> Self {
        self.rate_limiter = rate_limiter;
        self
    }

    pub(crate) fn token_list_url(mut self, url: impl Into<String>) -> Self {
        self.token_list_url = Some(url.into());
        self
    }

    pub(crate) fn raindex_config(mut self, config: crate::raindex::RaindexProvider) -> Self {
        self.raindex_config = Some(config);
        self
    }

    pub(crate) async fn build(self) -> Client {
        let id = uuid::Uuid::new_v4();
        let pool = crate::db::init(&format!("sqlite:file:{id}?mode=memory&cache=shared"))
            .await
            .expect("database init");

        let token_list_url = match self.token_list_url {
            Some(url) => url,
            None => mock_token_list_url().await,
        };

        let raindex_config = match self.raindex_config {
            Some(config) => config,
            None => {
                let registry_url = match self.raindex_registry_url {
                    Some(url) => url,
                    None => mock_raindex_registry_url().await,
                };
                crate::raindex::RaindexProvider::load(&registry_url)
                    .await
                    .expect("mock raindex config from registry url")
            }
        };

        let rocket = crate::rocket(pool, self.rate_limiter, raindex_config)
            .expect("valid rocket instance")
            .manage(crate::routes::tokens::TokensConfig::with_url(
                token_list_url,
            ));

        Client::tracked(rocket).await.expect("valid client")
    }
}

async fn mock_token_list_url() -> String {
    const BODY: &str = r#"{"tokens":[{"chainId":8453,"address":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","name":"USD Coin","symbol":"USDC","decimals":6}]}"#;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock token server");
    let addr = listener.local_addr().expect("mock token server address");
    let response = format!(
        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{BODY}",
        BODY.len()
    );

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };

            let response = response.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
                let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
            });
        }
    });

    format!("http://{addr}")
}

pub(crate) async fn mock_raindex_config() -> crate::raindex::RaindexProvider {
    let registry_url = mock_raindex_registry_url().await;
    crate::raindex::RaindexProvider::load(&registry_url)
        .await
        .expect("mock raindex config")
}

pub(crate) async fn mock_invalid_raindex_config() -> crate::raindex::RaindexProvider {
    let registry_url = mock_raindex_registry_url_with_settings("not valid yaml: [").await;
    crate::raindex::RaindexProvider::load(&registry_url)
        .await
        .expect("mock invalid raindex config")
}

pub(crate) async fn mock_raindex_registry_url() -> String {
    let settings = r#"version: 4
networks:
  base:
    rpcs:
      - https://mainnet.base.org
    chain-id: 8453
    currency: ETH
subgraphs:
  base: https://api.goldsky.com/api/public/project_clv14x04y9kzi01saerx7bxpg/subgraphs/ob4-base/0.9/gn
orderbooks:
  base:
    address: 0xd2938e7c9fe3597f78832ce780feb61945c377d7
    network: base
    subgraph: base
    deployment-block: 0
deployers:
  base:
    address: 0xC1A14cE2fd58A3A2f99deCb8eDd866204eE07f8D
    network: base
tokens:
  token1:
    address: 0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913
    network: base
"#;
    mock_raindex_registry_url_with_settings(settings).await
}

pub(crate) async fn mock_raindex_registry_url_with_settings(settings: &str) -> String {
    let settings = settings.to_string();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock registry server");
    let addr = listener.local_addr().expect("mock registry server address");

    let registry_body = format!("http://{addr}/settings.yaml");
    let settings_body = settings.to_string();

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };

            let registry_body = registry_body.clone();
            let settings_body = settings_body.clone();

            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = tokio::io::AsyncReadExt::read(&mut socket, &mut buf)
                    .await
                    .unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);

                let body = if request.contains("/settings.yaml") {
                    &settings_body
                } else {
                    &registry_body
                };

                let response = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
            });
        }
    });

    format!("http://{addr}/registry.txt")
}

pub(crate) async fn seed_api_key(client: &Client) -> (String, String) {
    let key_id = uuid::Uuid::new_v4().to_string();
    let secret = uuid::Uuid::new_v4().to_string();
    let hash = crate::auth::hash_secret(&secret).expect("hash secret");

    let pool = client
        .rocket()
        .state::<crate::db::DbPool>()
        .expect("pool in state");
    sqlx::query("INSERT INTO api_keys (key_id, secret_hash, label, owner) VALUES (?, ?, ?, ?)")
        .bind(&key_id)
        .bind(&hash)
        .bind("test-key")
        .bind("test-owner")
        .execute(pool)
        .await
        .expect("insert api key");

    (key_id, secret)
}

pub(crate) fn basic_auth_header(key_id: &str, secret: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{key_id}:{secret}"));
    format!("Basic {encoded}")
}

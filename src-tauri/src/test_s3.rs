//! Local HTTP fault fixture used by actual SDK/NFS production paths.
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    task::JoinHandle,
};

#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
impl Response {
    pub fn empty(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }
    pub fn xml(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/xml".into())],
            body: body.as_bytes().to_vec(),
        }
    }
    pub fn header(mut self, name: &str, value: impl ToString) -> Self {
        self.headers.push((name.into(), value.to_string()));
        self
    }
}
pub struct Fixture {
    pub endpoint: String,
    pub client: aws_sdk_s3::Client,
    pub requests: Arc<Mutex<Vec<Request>>>,
    server: JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

pub async fn serve<F, Fut>(handler: F) -> Fixture
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let client = crate::providers::s3_client::create_s3_client(
        &crate::providers::s3_client::S3ClientConfig {
            access_key_id: "fixture",
            secret_access_key: "fixture-secret",
            region: "us-east-1",
            endpoint_url: Some(&endpoint),
            force_path_style: true,
        },
    )
    .unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let handler = Arc::new(handler);
    let server = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((mut socket,_)) = accepted else { break; };
                    let handler = handler.clone(); let seen = seen.clone();
                    connections.spawn(async move {
                        let Some(request) = read_request(&mut socket).await else { return; };
                        let is_head = request.method == "HEAD";
                        seen.lock().unwrap().push(request.clone());
                        let response = handler(request).await;
                        let mut headers = format!("HTTP/1.1 {} fixture\r\nconnection: close\r\n", response.status);
                        if !response.headers.iter().any(|(key,_)| key.eq_ignore_ascii_case("content-length")) { headers.push_str(&format!("content-length: {}\r\n",response.body.len())); }
                        for (name,value) in response.headers { headers.push_str(&format!("{name}: {value}\r\n")); }
                        headers.push_str("\r\n");
                        let _ = socket.write_all(headers.as_bytes()).await;
                        if !is_head { let _ = socket.write_all(&response.body).await; }
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    Fixture {
        endpoint,
        client,
        requests,
        server,
    }
}

async fn line(socket: &mut TcpStream) -> Option<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let byte = socket.read_u8().await.ok()?;
        line.push(byte);
        if line.ends_with(b"\r\n") {
            line.truncate(line.len() - 2);
            return Some(line);
        }
        if line.len() > 64 * 1024 {
            return None;
        }
    }
}
async fn read_request(socket: &mut TcpStream) -> Option<Request> {
    let first = String::from_utf8(line(socket).await?).ok()?;
    let mut fields = first.split_whitespace();
    let method = fields.next()?.to_string();
    let path = fields.next()?.to_string();
    let mut headers = HashMap::new();
    loop {
        let row = line(socket).await?;
        if row.is_empty() {
            break;
        }
        let row = String::from_utf8(row).ok()?;
        let (name, value) = row.split_once(':')?;
        headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
    }
    if headers
        .get("expect")
        .is_some_and(|v| v.eq_ignore_ascii_case("100-continue"))
    {
        socket
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .await
            .ok()?;
    }
    let mut body = Vec::new();
    if headers
        .get("transfer-encoding")
        .is_some_and(|v| v.contains("chunked"))
    {
        loop {
            let size = String::from_utf8(line(socket).await?).ok()?;
            let size = usize::from_str_radix(size.split(';').next()?, 16).ok()?;
            if size == 0 {
                while !line(socket).await?.is_empty() {}
                break;
            }
            if body.len() + size > 16 * 1024 * 1024 {
                return None;
            }
            let start = body.len();
            body.resize(start + size, 0);
            socket.read_exact(&mut body[start..]).await.ok()?;
            if !line(socket).await?.is_empty() {
                return None;
            }
        }
    } else if let Some(length) = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
    {
        if length > 16 * 1024 * 1024 {
            return None;
        }
        body.resize(length, 0);
        socket.read_exact(&mut body).await.ok()?;
    }
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}

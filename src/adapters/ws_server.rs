use crate::message::Message;
use crate::actor::{Actor, Addr, ActorContext};
use crate::Config;
use crate::adapters::ws_conn::WsConn;

use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::{Arc};
use std::fs::File;
use std::io::Read;
use tokio::sync::RwLock;

use futures_util::StreamExt;
use log::{info};
use tokio_native_tls::native_tls::{Identity};
use tokio::net::TcpListener;

use tokio_tungstenite::MaybeTlsStream;

type Clients = Arc<RwLock<HashSet<Addr>>>;

pub struct WsServer {
    config: Config,
    clients: Clients
}
impl WsServer {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            clients: Clients::default()
        }
    }

    async fn handle_stream(stream: MaybeTlsStream<tokio::net::TcpStream>, ctx: &ActorContext, clients: Clients) {
        let ws_stream = match tokio_tungstenite::accept_async(stream).await {
            Ok(s) => s,
            Err(_e) => {
                // suppress errors from receiving normal http requests
                // error!("Error during the websocket handshake occurred: {}", e);
                return;
            }
        };

        let (sender, receiver) = ws_stream.split();

        let conn = WsConn::new(sender, receiver);
        let addr = ctx.start_actor(Box::new(conn));
        clients.write().await.insert(addr);
    }
}
#[async_trait]
impl Actor for WsServer {
    async fn handle(&mut self, msg: Message, _ctx: &ActorContext) {
        for conn in self.clients.read().await.iter() {
            if msg.is_from(conn) {
                continue;
            }
            if let Err(_) = conn.sender.send(msg.clone()) {
                self.clients.write().await.remove(conn);
            }
        }
    }

    async fn pre_start(&mut self, ctx: &ActorContext) {
        let addr = format!("0.0.0.0:{}", self.config.websocket_server_port).to_string();
        let clients = self.clients.clone();
        let ctx = ctx.clone();

        // Create the event loop and TCP listener we'll accept connections on.
        let try_socket = TcpListener::bind(&addr).await;
        let listener = try_socket.expect("Failed to bind");
        info!("Listening on: {}", addr);

        if let Some(cert_path) = &self.config.cert_path {
            let mut cert_file = File::open(cert_path).unwrap();
            let mut cert = vec![];
            cert_file.read_to_end(&mut cert).unwrap();

            let key_path = self.config.key_path.as_ref().unwrap();
            let mut key_file = File::open(key_path).unwrap();
            let mut key = vec![];
            key_file.read_to_end(&mut key).unwrap();

            let identity = Identity::from_pkcs8(&cert, &key).unwrap();
            let acceptor = tokio_native_tls::native_tls::TlsAcceptor::new(identity).unwrap();
            let acceptor = tokio_native_tls::TlsAcceptor::from(acceptor);
            let acceptor = Arc::new(acceptor);

            ctx.clone().abort_on_stop(tokio::spawn(async move {
                loop {
                    if let Ok((stream, _)) = listener.accept().await {
                        let acceptor = acceptor.clone();
                        let clients = clients.clone();
                        let ctx = ctx.clone();
                        tokio::spawn(async move {
                            let stream = acceptor.accept(stream).await;
                            match stream {
                                Ok(stream) => {
                                    Self::handle_stream(MaybeTlsStream::NativeTls(stream), &ctx, clients.clone()).await;
                                },
                                _ => {}
                            }
                        });
                    }
                }
            }));
        } else {
            ctx.clone().abort_on_stop(tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    Self::handle_stream(MaybeTlsStream::Plain(stream), &ctx, clients.clone()).await;
                }
            }));
        }
    }

    async fn stopping(&mut self, _context: &ActorContext) {
        info!("WsServer stopping");
    }
}

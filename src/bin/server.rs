use futures_util::sink::SinkExt;
use futures_util::stream::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::sync::broadcast::{channel, Sender};
use tokio_websockets::{Message, ServerBuilder, WebSocketStream};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IncomingWsMessage {
    message_type: String,
    data_array: Option<Vec<String>>,
    data: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OutgoingWsMessage {
    message_type: String,
    data_array: Option<Vec<String>>,
    data: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatMessageData {
    from: String,
    message: String,
}

async fn broadcast_users(
    bcast_tx: &Sender<String>,
    users: Arc<Mutex<HashMap<SocketAddr, String>>>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let users_guard = users.lock().await;
    let user_list = users_guard.values().cloned().collect::<Vec<_>>();
    drop(users_guard);

    let users_payload = OutgoingWsMessage {
        message_type: "users".to_owned(),
        data_array: Some(user_list),
        data: None,
    };
    bcast_tx.send(serde_json::to_string(&users_payload)?)?;
    Ok(())
}

async fn handle_connection(
    addr: SocketAddr,
    mut ws_stream: WebSocketStream<TcpStream>,
    bcast_tx: Sender<String>,
    users: Arc<Mutex<HashMap<SocketAddr, String>>>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut bcast_rx = bcast_tx.subscribe();

    loop {
        tokio::select! {
            incoming = ws_stream.next() => {
                match incoming {
                    Some(Ok(msg)) => {
                        if let Some(text) = msg.as_text() {
                            let parsed: IncomingWsMessage = match serde_json::from_str(text) {
                                Ok(p) => p,
                                Err(err) => {
                                    eprintln!("Invalid JSON from {addr:?}: {err}");
                                    continue;
                                }
                            };

                            match parsed.message_type.as_str() {
                                "register" => {
                                    let username = parsed
                                        .data
                                        .unwrap_or_else(|| format!("{addr:?}"));
                                    println!("Register user {username} from {addr:?}");
                                    {
                                        let mut users_guard = users.lock().await;
                                        users_guard.insert(addr, username);
                                    }
                                    broadcast_users(&bcast_tx, users.clone()).await?;
                                }
                                "message" => {
                                    let message_text = parsed.data.unwrap_or_default();
                                    let sender_name = {
                                        let users_guard = users.lock().await;
                                        users_guard
                                            .get(&addr)
                                            .cloned()
                                            .unwrap_or_else(|| format!("{addr:?}"))
                                    };

                                    println!("From client {sender_name} ({addr:?}): {message_text}");

                                    let message_data = ChatMessageData {
                                        from: sender_name,
                                        message: message_text,
                                    };
                                    let wrapped = OutgoingWsMessage {
                                        message_type: "message".to_owned(),
                                        data: Some(serde_json::to_string(&message_data)?),
                                        data_array: None,
                                    };
                                    bcast_tx.send(serde_json::to_string(&wrapped)?)?;
                                }
                                _ => {
                                    let _ = parsed.data_array;
                                }
                            }
                        }
                    }
                    Some(Err(err)) => {
                        let _ = users.lock().await.remove(&addr);
                        let _ = broadcast_users(&bcast_tx, users.clone()).await;
                        return Err(err.into());
                    }
                    None => {
                        let _ = users.lock().await.remove(&addr);
                        let _ = broadcast_users(&bcast_tx, users.clone()).await;
                        return Ok(());
                    }
                }
            }
            msg = bcast_rx.recv() => {
                ws_stream.send(Message::text(msg?)).await?;
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let (bcast_tx, _) = channel(16);
    let users = Arc::new(Mutex::new(HashMap::<SocketAddr, String>::new()));

    let listener = TcpListener::bind("127.0.0.1:8080").await?;
    println!("listening on port 8080");

    loop {
        let (socket, addr) = listener.accept().await?;
        println!("New connection from {addr:?}");
        let bcast_tx = bcast_tx.clone();
        let users = users.clone();

        tokio::spawn(async move {
            let (_req, ws_stream) = ServerBuilder::new().accept(socket).await?;
            handle_connection(addr, ws_stream, bcast_tx, users).await
        });
    }
}

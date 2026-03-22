use std::io::{BufRead as _, Error};

use clap::Parser;
use futures_util::{future, SinkExt, StreamExt, TryStreamExt};
use log::info;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::broadcast,
};
use tokio_tungstenite::tungstenite::{
    error::{Error as TsError, ProtocolError},
    Message,
};

#[derive(Parser, Debug)]
struct Args {
    #[arg(short, long, default_value = "127.0.0.1:8080")]
    listen: String,
    #[arg(short, long)]
    file: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let _ = env_logger::try_init();
    let args = Args::parse();

    let (s, mut r) = broadcast::channel(16);
    tokio::spawn(read_file_slowly(args.file, s));

    // Create the event loop and TCP listener we'll accept connections on.
    let try_socket = TcpListener::bind(&args.listen).await;
    let listener = try_socket.expect("Failed to bind");
    info!("Listening on: {}", args.listen);

    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(accept_connection(stream, r.resubscribe()));
    }

    Ok(())
}

async fn read_file_slowly(path: String, sender: broadcast::Sender<String>) {
    let file = std::fs::File::open(path).unwrap();
    let reader = std::io::BufReader::new(file);
    for line in reader.lines() {
        let line = line.unwrap();
        sender.send(line).unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    }
}

async fn accept_connection(stream: TcpStream, mut receiver: broadcast::Receiver<String>) {
    let addr = stream
        .peer_addr()
        .expect("connected streams should have a peer address");

    let ws_stream = tokio_tungstenite::accept_async(stream)
        .await
        .expect("Error during the websocket handshake occurred");

    log::info!("New WebSocket connection: {}", addr);

    let (mut write, read) = ws_stream.split();
    let mut msgs = read.try_filter(|msg| future::ready(msg.is_text() || msg.is_binary()));

    loop {
        tokio::select! {
            msg = msgs.next() => {
                match msg {
                    Some(Ok(ref m)) => log::debug!("Received message {m:?} from {addr}"),
                    Some(Err(TsError::Protocol(ProtocolError::ResetWithoutClosingHandshake))) => break,
                    Some(Err(e)) => log::error!("Error from client: {e:?}"),
                    _ => break,
                }
            },
            to_send = receiver.recv() => {
                match to_send {
                    Ok(m) => write.send(Message::Text(m.into())).await.expect("Should send"),
                    Err(e) => {
                        log::error!("Could not receive message from channel: {e:?}");
                        break;
                    },
                }
            },
        }
    }

    log::debug!("Done with client {addr}");
}

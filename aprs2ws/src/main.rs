use std::io::{BufRead as _, Error, Read, Seek};

use anyhow::Result;
use aprs::Report;
use clap::Parser;
use futures_util::{future, SinkExt, StreamExt, TryStreamExt};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::broadcast,
};
use tokio_tungstenite::tungstenite::{
    error::{Error as TsError, ProtocolError},
    Message,
};

mod formatter;

use formatter::ReportFormatter;

#[derive(Parser, Debug)]
struct Args {
    #[arg(short, long, default_value = "127.0.0.1:8080")]
    listen: String,
    #[arg(short, long, help = "APRS to read, for debugging purposes.")]
    file: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let _ = env_logger::try_init();
    let args = Args::parse();

    let formatter = ReportFormatter::new("LFQD (real-time)".to_owned()).await;

    let (s, r) = broadcast::channel(16);
    tokio::spawn(read_file_slowly(args.file, s, formatter));

    // Create the event loop and TCP listener we'll accept connections on.
    let try_socket = TcpListener::bind(&args.listen).await;
    let listener = try_socket.expect("Failed to bind");
    log::info!("Listening on: {}", args.listen);

    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(accept_connection(stream, r.resubscribe()));
    }

    Ok(())
}

async fn read_file_slowly(
    path: String,
    sender: broadcast::Sender<String>,
    formatter: ReportFormatter,
) -> std::io::Result<()> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    loop {
        for line in reader.by_ref().lines() {
            let line = line?;
            if let Ok(Report::PositionReport(report)) = line.parse::<Report>() {
                if sender.send(formatter.format(&report)).is_err() {
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        reader.seek(std::io::SeekFrom::Start(0))?;
    }
}

async fn accept_connection(
    stream: TcpStream,
    mut receiver: broadcast::Receiver<String>,
) -> Result<()> {
    let addr = stream.peer_addr()?;

    let ws_stream = tokio_tungstenite::accept_async(stream).await?;

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
                    Ok(m) => write.send(Message::Text(m.into())).await?,
                    Err(broadcast::error::RecvError::Closed) => {
                        write.send(Message::Close(None)).await?;
                        break;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => (),
                }
            },
        }
    }

    log::debug!("Done with client {addr}");
    Ok(())
}

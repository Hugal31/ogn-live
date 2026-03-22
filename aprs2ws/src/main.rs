use std::collections::HashMap;
use std::io::{BufRead as _, Error, Read, Seek};

use aprs::{PositionReport, Report};
use clap::Parser;
use futures_util::{future, SinkExt, StreamExt, TryStreamExt};
use ogn::ddb::Device;
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

struct ReportFormatter {
    pub receiver: String,
    pub database: HashMap<String, Device>,
}

impl ReportFormatter {
    pub async fn new(receiver: String) -> Self {
        Self {
            receiver,
            database: Self::get_ogn_ddb().await,
        }
    }

    async fn get_ogn_ddb() -> HashMap<String, Device> {
        use ogn::ddb::{index_by_id, read_database, OGN_DDB_URL};
        let body = reqwest::get(OGN_DDB_URL)
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        read_database(body.as_bytes()).map(index_by_id).unwrap()
    }

    /// Output in this format:
    /// ```
    /// <m a="46.1,5.1,OI,G-EZOI,2371,11:16:45,20,135,482,0.0,0,Grenoble,0,d4de7516"/>
    /// ```
    /// Lat,Lon,Imat Short, Imat, Alt(m), Last heard time (which TZ?),ddf,Track,Ground speed (km/h),Vz (m/s),Type,Receiver,shownId,id
    /// ddf = secs since last heard?
    /// Type 0 = unknown
    /// Type 1 = Glider/Motorglider
    /// Type 3 = Helicopter
    /// Type 6 = Handglider
    /// See ogn.js "ftype" for the rest.
    ///
    pub fn format(&self, report: &PositionReport) -> String {
        log::debug!("Formatting {report:?}");
        let position = report.point();
        let id = report.id();
        let device = id.as_ref().and_then(|i| self.database.get(&i[2..]));
        if let Some(device) = device {
            log::debug!("Device is {device:?}");
        }
        let id = id.as_deref().unwrap_or("").to_ascii_lowercase();
        let immat = device.map(|d| &d.registration).unwrap_or(&id);
        let short_immat = if let Some(device) = device {
            device.common_name.clone()
        } else if immat.len() < 2 {
            "??".to_string()
        } else {
            format!(
                "_{}{}",
                id.chars().nth(id.len() - 2).unwrap(),
                id.chars().nth(id.len() - 1).unwrap()
            )
        };
        let alt_m = report.altitude().unwrap_or(0.) * 0.3048;
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<markers>
<m a="{lat:.5},{lon:.5},{short_immat},{immat},{alt},{time},0,{track},{gs},{vz},{typ},{recv},{shown_id},{id}"/>
</markers>"#,
            lat = position.y(),
            lon = position.x(),
            short_immat = short_immat,
            immat = immat,
            alt = alt_m,
            time = report
                .timestamp
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            track = report.course().unwrap_or(0),
            // TODO Check unit
            gs = report.speed().unwrap_or(0.) * 1.852,
            vz = report.climb_rate().unwrap_or(0.) * 0.00508,
            typ = Self::guess_type(&report.symbol, device),
            recv = self.receiver,
            shown_id = device.map(|d| &d.id).unwrap_or(&id),
            id = id,
        )
    }

    fn guess_type(symbol: &[u8; 2], device: Option<&Device>) -> i32 {
        // TODO Find a more correct heuristic...
        if device.is_some() {
            return 1;
        }

        match [symbol[0] as char, symbol[1] as char] {
            ['/', 'g'] => 1,
            _ => 0,
        }
    }
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
) {
    let file = std::fs::File::open(path).unwrap();
    let mut reader = std::io::BufReader::new(file);
    loop {
        for line in reader.by_ref().lines() {
            let line = line.unwrap();
            if let Ok(Report::PositionReport(report)) = line.parse::<Report>() {
                sender.send(formatter.format(&report)).unwrap();
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        reader.seek(std::io::SeekFrom::Start(0)).unwrap();
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
                    Err(broadcast::error::RecvError::Closed) => {
                        write.send(Message::Close(None)).await.expect("Should send");
                        break;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => (),
                }
            },
        }
    }

    log::debug!("Done with client {addr}");
}

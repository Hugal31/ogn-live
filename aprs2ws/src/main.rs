use flightlogr::aprs::client::APRSClient;
use websocket::r#async::futures::{Future, Stream};
use websocket::r#async::server::upgrade::IntoWs;
use websocket::r#async::{TcpListener, TcpStream};
use websocket::sync::Client;

// TODO Connect to an APRS stream that doesn't require actual user/password.
// Stream everything to a WS.

fn main() {
    let mut runtime = tokio::runtime::Builder::new().build().unwrap();
    let executor = runtime.executor();
    let addr = "127.0.0.1:80".parse().unwrap();
    let listener = TcpListener::bind(&addr).unwrap();

    let websocket_clients = listener
        .incoming()
        .map_err(|e| e.into())
        .and_then(|stream| stream.into_ws().map_err(|e| e.3))
        .map(|upgrade| {
            if upgrade.protocols().iter().any(|p| p == "super-cool-proto") {
                let accepted = upgrade
                    .use_protocol("super-cool-proto")
                    .accept()
                    .map(|_| ())
                    .map_err(|_| ());

                executor.spawn(accepted);
            } else {
                let rejected = upgrade.reject().map(|_| ()).map_err(|_| ());

                executor.spawn(rejected);
            }
        });
}

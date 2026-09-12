mod airplay;
mod cast;
mod client;
mod daemon;
mod discovery;
mod dlna;
mod firewall;
mod media_server;
mod mpris;
mod protocol;
mod settings;
mod state;
mod subtitles;
mod util;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--client") {
        let client_args: Vec<String> = args.into_iter().skip(1).collect();
        client::main(&client_args);
    } else {
        daemon::main();
    }
}

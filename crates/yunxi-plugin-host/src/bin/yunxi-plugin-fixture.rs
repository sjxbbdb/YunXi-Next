//! Small protocol-only process used by dynamic plugin integration tests.

use std::env;
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::process;
use std::time::Duration;

use yunxi_protocol::{
    CONNECT_ADDRESS_ENV, CapabilityDescriptor, HostMessage, InvocationResponse, PluginMessage,
    connect_plugin,
};

fn argument(arguments: &[String], name: &str, default: Option<&str>) -> Result<String, String> {
    arguments
        .windows(2)
        .find_map(|pair| (pair[0] == name).then(|| pair[1].clone()))
        .or_else(|| default.map(str::to_string))
        .ok_or_else(|| format!("missing required argument {name}"))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let plugin_id = argument(&arguments, "--id", None)?;
    let capability_id = argument(&arguments, "--capability", None)?;
    let plugin_version = argument(&arguments, "--version", Some("1.0.0"))?;
    let mode = argument(&arguments, "--mode", Some("echo"))?;

    if mode == "malformed" {
        let address = env::var(CONNECT_ADDRESS_ENV)?.parse::<SocketAddr>()?;
        let mut stream = TcpStream::connect(address)?;
        stream.write_all(b"{dynamic fixture frame\n")?;
        return Ok(());
    }

    let capability = CapabilityDescriptor::new(&capability_id, 1)?;
    let mut session = connect_plugin(
        plugin_id,
        "YunXi dynamic integration fixture",
        plugin_version,
        vec![capability],
        Duration::from_secs(2),
    )?;

    if mode == "crash-after-ready" {
        process::exit(42);
    }

    loop {
        match session.receive()? {
            HostMessage::Invoke { request } => {
                let payload = request.decode_payload::<String>()?;
                let response = InvocationResponse::encode(request.request_id(), &payload)?;
                session.send(&PluginMessage::InvocationCompleted { response })?;
            }
            HostMessage::Cancel { .. } => {}
            HostMessage::Shutdown => return Ok(()),
            HostMessage::Welcome { .. } => return Err("unexpected second welcome".into()),
        }
    }
}

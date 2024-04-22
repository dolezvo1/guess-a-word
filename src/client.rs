
use std::env;
use std::net::TcpStream;
use std::net::SocketAddr;
use std::sync::mpsc;


mod client_worker;
mod util;
mod client_util;

use crate::client_worker::ClientWorker;
use crate::util::{parse_arg, GuessProtocol as Ptcl, ProtocolReader, ProtocolWriter};
use crate::client_util::{spawn_stdin_listener, ClientInternalMessage as IMsg};


static ACCEPTED_OPTIONS: [&str; 2] = [
    "--tcp-address",
    "--unix-socket-path",
];

#[tokio::main]
async fn main() {
    let args: Vec<_> = env::args().collect();
    
    let result = match (parse_arg::<String>("--tcp-address", &args, &ACCEPTED_OPTIONS),
           parse_arg::<String>("--unix-socket-path", &args, &ACCEPTED_OPTIONS)) {
        // Both or neither destinations provided, return with 1
        (Some(_), Some(_)) | (None, None) => {
            Err("Exactly one destination must be provided (`--tcp-address ADDRESS:PORT` or `--unix-socket-path PATH`, not both).")
        },
        // Connect using TCP
        (Some(tcp_address), None) => {
            let tcp_stream = tcp_address.parse::<SocketAddr>()
                               .map_err(|_| "Invalid TCP target")
                               .and_then(|addr|
                                    TcpStream::connect(addr)
                                    .map_err(|_| "TCP connection could not be established")
                               );
            tcp_stream.and_then(|stream|
                stream.try_clone()
                      .map_err(|_| "TCP stream could not be cloned")
                      .and_then(|clone| comm((Box::new(stream), Box::new(clone))))
            )
        },
        
        // Connect using Unix Sockets if possible
        #[cfg_attr(not(target_family="unix"), allow(unused_variables))]
        (None, Some(unix_socket_path)) => {
            #[cfg(not(target_family="unix"))] {
                Err("Your system does not support Unix Sockets. Use TCP (`--tcp-address ADDRESS:PORT`).")
            }
            #[cfg(target_family="unix")] {
                let unix_stream = UnixStream::connect(unix_socket_path)
                                .map_err(|_| "Unix Socket connection could not be established");
                unix_stream.and_then(|stream|
                    stream.try_clone()
                          .map_err(|_| "Unix socket stream could not be cloned")
                          .and_then(|clone| comm(Box::new(stream), Box::new(clone)))
                )
            }
        },
    };
    if let Err(e) = result {
        println!("Error: {}", e);
    }
}

fn comm<R, W>((server_r, server_w): (Box<R>, Box<W>)) -> Result<(), &'static str>
    where R: ProtocolReader<Ptcl> + Send + 'static,
          W: ProtocolWriter<Ptcl> + Send
{
    // Establish joint message channel
    let (tx, joint_rx) = mpsc::channel::<IMsg<Ptcl>>();
    
    let mut cw = ClientWorker::new((&*server_r, &*server_w))?;
    
    println!("You were assigned ID {}", cw.id());
    println!("Available commands:\n\tlist - List available opponents\n\tconnect ID WORD - Connect to an opponent");
    
    // Add network listener to the joint channel
    let _ = IMsg::<Ptcl>::spawn_network_listener(server_r, tx.clone());
    let _ = spawn_stdin_listener(tx.clone());
    
    // Handle all messages in the joint channel until error
    while let Ok(msg) = joint_rx.recv() {
        cw.handle_message(msg, &*server_w)?
    }
    unreachable!();
}

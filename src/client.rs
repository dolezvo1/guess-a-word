
use std::env;
use std::io;
use std::io::Write;
use std::net::TcpStream;
use std::net::SocketAddr;
use std::sync::mpsc;
use std::sync::mpsc::Sender;
use tokio::task::{spawn_blocking, JoinHandle};

mod util;
use crate::util::{
    parse_arg,
    ClientState, InternalMessage as IMsg, GuessProtocol as Ptcl,
    ProtocolReader, ProtocolWriter,
    spawn_network_listener,
};

static ACCEPTED_OPTIONS: [&str; 2] = [
    "--tcp-address",
    "--unix-socket-path",
];

#[tokio::main]
async fn main() -> Result<(), &'static str> {
    let args: Vec<_> = env::args().collect();
    
    match (parse_arg::<String>("--tcp-address", &args, &ACCEPTED_OPTIONS),
           parse_arg::<String>("--unix-socket-path", &args, &ACCEPTED_OPTIONS)) {
        // Both or neither destinations provided, return with 1
        (Some(_), Some(_)) | (None, None) => {
            return Err("Exactly one destination must be provided (`--tcp-address ADDRESS:PORT` or `--unix-socket-path PATH`, not both).");
        },
        // Connect using TCP
        (Some(tcp_address), None) => {
            let tcp_stream = TcpStream::connect(
                                    tcp_address.parse::<SocketAddr>()
                                               .map_err(|_| "Invalid TCP target")?
                                ).map_err(|_| "TCP connection could not be established")?;
            tcp_stream.try_clone().map(|clone|
                comm((Box::new(tcp_stream), Box::new(clone)))
            ).map_err(|_| "TCP stream could not be cloned")?
        },
        // Connect using Unix Sockets if possible
        (None, Some(unix_socket_path)) => {
            #[cfg(not(target_family="unix"))] {
                return Err("Your system does not support Unix Sockets. Use TCP.");
            }
            #[cfg(target_family="unix")] {
                let unix_stream = UnixStream::connect(unix_socket_path)
                                .map_err(|_| "Unix Socket connection could not be established")?;
                comm(Box::new(unix_stream))
            }
        },
    }
}

macro_rules! input { ($prompt: expr) => {{
    print!("{}", $prompt);
    let _ = io::stdout().flush();
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
    
    line.strip_suffix(if cfg!(windows) {"\r\n"} else {"\n"}).unwrap().to_string()
}}}

fn comm<R, W>((server_r, server_w): (Box<R>, Box<W>)) -> Result<(), &'static str>
    where R: ProtocolReader<Ptcl> + Send + 'static,
          W: ProtocolWriter<Ptcl> + Send
{
    // Start of the communication: Server should send Hello, Client should respond with a password
    match server_r.read() {
        Ok(Ptcl::ServerHello) => {},
        _ => return Err("Incorrect server response"),
    }
    
    let _ = server_w.write(&Ptcl::ClientPassword(input!("Enter server password: ")));
    
    // Read assigned id
    let assigned_id = match server_r.read() {
        Ok(Ptcl::ConnectionEstablished(id)) => id,
        _ => return Err("Incorrect server response"),
    };
    
    println!("You were assigned ID {}", assigned_id);
    println!("Available commands:\n\tlist - List available opponents\n\tconnect ID WORD - Connect to an opponent");
    
    // Create poll object listening to network and stdin
    let (tx, joint_rx) = mpsc::channel();
    let _ = spawn_network_listener(server_r, tx.clone());
    let _ = spawn_stdin_listener(tx.clone());
    
    let mut client_state = ClientState::Free;
    let mut opponent_id = None;
    
    macro_rules! clean_client_state { () => {
        client_state = ClientState::Free;
        opponent_id = None;
    }}
    
    while let Ok(msg) = joint_rx.recv() {
        match msg {
            // Connection to server was lost
            IMsg::Error => {
                println!("connection to server was lost");
                break
            },
            // Invalid message was read, inform server?
            IMsg::Network(Err(_)) => {
                let _ = server_w.write(&Ptcl::UnrecognizedMessageError);
            },
            // Handle valid messages from server
            IMsg::Network(Ok(msg)) => match (msg, &opponent_id) {
                (Ptcl::ServerOpponentList(opponent_list), None) => {
                    if !opponent_list.is_empty() {
                        println!("Available opponents ({}):", opponent_list.len());
                        for opponent in opponent_list {
                            println!("{}", opponent);
                        }
                    } else {
                        println!("No available opponents");
                    }
                },
                (Ptcl::OpponentConnectionEstablished(id, state), None) => {
                    println!("You were connected to user {} as a {}", id,
                        match &state {
                            ClientState::Riddlemaker(_) => "riddlemaker",
                            ClientState::Guesser => "guesser",
                            _ => panic!(),
                        }
                    );
                    client_state = state;
                    opponent_id = Some(id);
                },
                (Ptcl::OpponentConnectionNotEstablished(oid), None) => {
                    println!("Connection to user {} could not be established", oid);
                },
                (Ptcl::ServerOpponentDisconnected, Some(_)) => {
                    println!("Opponent disconnected");
                    clean_client_state!();
                },
                (Ptcl::ClientGuess(w), Some(id)) => println!("User {} guessed '{}'", id, w),
                (Ptcl::ClientHint(w), Some(id)) => println!("User {} hinted '{}'", id, w),
                (Ptcl::ServerWordFound, Some(_)) => {
                    match client_state {
                        ClientState::Riddlemaker(_) => println!("Opponent found the word!"),
                        ClientState::Guesser => println!("You found the word!"),
                        _ => {}
                    }
                    clean_client_state!();
                },
                a => println!("{:?}", a),
                // You were connected to [id] as a [role]
            },
            // Handle keyboard input
            IMsg::StdIn(input) => match &client_state {
                ClientState::Free => match input.splitn(3, ' ').collect::<Vec<_>>()[..] {
                    ["list"] => { let _ = server_w.write(&Ptcl::ClientListOpponents); },
                    ["connect", c, w] => {
                        let _ = server_w.write(&Ptcl::ClientSelectOpponent(c.to_string(), w.to_string()));
                    },
                    _ => { println!("unrecognized command"); },
                },
                ClientState::Riddlemaker(_) => { let _ = server_w.write(&Ptcl::ClientHint(input)); },
                ClientState::Guesser => { let _ = server_w.write(&Ptcl::ClientGuess(input)); }
            },
            _ => {},
        }
    }
    
    Ok(())
}

fn spawn_stdin_listener(transmitter: Sender<IMsg<Ptcl>>) -> JoinHandle<()> {
    spawn_blocking(move || { loop {
        let mut buffer = String::new();
        io::stdin().read_line(&mut buffer).unwrap();
        buffer = buffer.strip_suffix(if cfg!(windows) {"\r\n"} else {"\n"})
                       .unwrap().to_string();
        if transmitter.send(IMsg::StdIn(buffer)).is_err() {
            break
        }
    }})
}

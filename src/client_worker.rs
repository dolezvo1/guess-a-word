
use std::io::Write;

use crate::protocol::{ClientState, GuessProtocol as Ptcl, ProtocolReader, ProtocolWriter};
use crate::client_util::{prompted_input, ClientInternalMessage as IMsg};

/// Client-side representation of client state
pub struct ClientWorker {
    id: String,
    client_state: ClientState,
    opponent_id: Option<String>,
}

impl ClientWorker {
    /// Create new connection to server, establish communication
    pub fn new<R,W>(
        (server_r, server_w): (&mut R, &mut W),
    ) -> Result<Self, &'static str>
        where R: ProtocolReader<Ptcl> + ?Sized,
              W: ProtocolWriter<Ptcl> + ?Sized
    {
        // Server should send Hello, Client should respond with a password
        match server_r.read() {
            Ok(Ptcl::ServerHello) => {},
            _ => return Err("Incorrect server response"),
        }
        
        let _ = server_w.write(&Ptcl::ClientPassword(
                                        prompted_input!("Enter server password: ")));
        
        // Read assigned id
        let id = match server_r.read() {
            Ok(Ptcl::ConnectionEstablished(id)) => id,
            _ => return Err("Incorrect server response"),
        };
        
        Ok(Self{id,client_state: ClientState::Free, opponent_id: None})
    }
    /// Get id assigned to the user
    pub fn id(&self) -> &str { &self.id }
    /// Handle a message from the joint channel
    pub fn handle_message<W>(
        &mut self,
        msg: IMsg<Ptcl>,
        server_w: &mut W,
    ) -> Result<(), &'static str>
        where W: ProtocolWriter<Ptcl> + ?Sized
    {
        match msg {
            // Connection to server was lost
            IMsg::Error => {
                return Err("Connection to server was lost");
            },
            // Invalid message was read, inform server?
            IMsg::Network(Err(_)) => {
                let _ = server_w.write(&Ptcl::UnrecognizedMessageError);
            },
            // Handle valid messages from server
            IMsg::Network(Ok(msg)) => match (msg, &self.opponent_id) {
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
                    self.client_state = state;
                    self.opponent_id = Some(id);
                },
                (Ptcl::OpponentConnectionNotEstablished(oid), None) => {
                    println!("Connection to user {} could not be established", oid);
                },
                (Ptcl::ServerOpponentDisconnected, Some(_)) => {
                    println!("Opponent disconnected");
                    self.reset_state();
                },
                (Ptcl::ClientGuess(w), Some(id)) => println!("User {} guessed '{}'", id, w),
                (Ptcl::ClientHint(w), Some(id)) => println!("User {} hinted '{}'", id, w),
                (Ptcl::ServerWordFound, Some(_)) => {
                    match self.client_state {
                        ClientState::Riddlemaker(_) => println!("Opponent found the word!"),
                        ClientState::Guesser => println!("You found the word!"),
                        _ => {}
                    }
                    self.reset_state();
                },
                a => println!("{:?}", a),
                // You were connected to [id] as a [role]
            },
            // Handle keyboard input
            IMsg::StdIn(input) => match &self.client_state {
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
        }
        Ok(())
    }
    fn reset_state(&mut self) {
        self.client_state = ClientState::Free;
        self.opponent_id = None;
    }
}

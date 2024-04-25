
use crate::protocol::{ClientState, GuessProtocol as Ptcl, ProtocolReader, ProtocolWriter};
use crate::util::{InternalMessage as IMsg};
use crate::{Sender, Store};

/// Server-side representation of a client.
pub struct ServerWorker {
    /// Reference to the server storage
    store: Store,
    /// Client id
    id: String,
    /// Client game state
    client_state: ClientState,
    /// Transmitter for the joint channel of the client
    tx: Sender,
    /// Transmitter for the joint channel of the opponent
    tx_to_opponent: Option<Sender>,
    /// Opponent word
    opponent_word: Option<String>,
}

impl ServerWorker {
    pub fn new<R,W>(
        (client_r, client_w): (&mut R, &mut W),
        password: String,
        store: Store,
        tx: Sender,
    ) -> Result<Self, ()>
        where R: ProtocolReader<Ptcl> + ?Sized,
              W: ProtocolWriter<Ptcl> + ?Sized
    {
        // "Upon connection - the server must send a message to the client - initiating the communication. Client upon receiving it - answers to the server with a password."
        let _ = client_w.write(&Ptcl::ServerHello);
        match client_r.read() {
            Ok(Ptcl::ClientPassword(p)) if p == password => {},
            _ => return Err(()),
        }
        
        // "This initial exchange then ends with server either disconnecting the client (wrong password) or assigning the client an ID and sending the ID back to the client."
        let id = {
            let mut lock = store.write().unwrap();
            let id = lock.id_generator;
            lock.id_generator += 1;
            let id = id.to_string();
            lock.available_clients.insert(id.clone(), (ClientState::Free, tx.clone()));
            id
        };
        let _ = client_w.write(&Ptcl::ConnectionEstablished(id.clone()));
        
        Ok(Self{store, id, tx, client_state: ClientState::Free,
                tx_to_opponent: None, opponent_word: None})
    }
    
    pub fn handle_message<W>(
        &mut self,
        msg: IMsg<Ptcl>,
        client_w: &mut W,
    ) -> Result<(),()>
        where W: ProtocolWriter<Ptcl> + ?Sized
    {
        match msg {
            // Client was likely disconnected, therefore terminate
            IMsg::Error => {
                if let Some(tx) = &self.tx_to_opponent {
                    let _ = tx.send(IMsg::OpponentDisconnected);
                }
                return Err(());
            },
            IMsg::OpponentDisconnected => {
                let _ = client_w.write(&Ptcl::ServerOpponentDisconnected);
                self.reset_state();
            }
            // Client got connected as a guesser
            IMsg::OpponentAssigned(tx, id, w) if self.client_state == ClientState::Free => {
                self.client_state = ClientState::Guesser;
                self.tx_to_opponent = Some(tx);
                self.opponent_word = Some(w);
                let _ = client_w.write(&Ptcl::OpponentConnectionEstablished(
                                                id.clone(), ClientState::Guesser));
            },
            IMsg::WordFound => {
                let _ = client_w.write(&Ptcl::ServerWordFound);
                self.reset_state();
            },
            // Resend messages from opponent to client
            IMsg::Opponent(msg) => {
                let _ = client_w.write(&msg);
            },
            // Invalid message was read, inform client
            IMsg::Network(Err(_)) => {
                let _ = client_w.write(&Ptcl::UnrecognizedMessageError);
            },
            // Process valid messages from client
            IMsg::Network(Ok(msg)) => match (&self.client_state, msg) {
                (_, Ptcl::ClientListOpponents) => {
                    let lock = self.store.read().unwrap();
                    let _ = client_w.write(&Ptcl::ServerOpponentList(
                        lock.available_clients.iter()
                            .filter_map(|(k,v)|
                                if *k != self.id && v.0 == ClientState::Free {
                                    Some(k.to_string())
                                } else {None}).collect::<Vec<String>>()));
                },
                (ClientState::Free, Ptcl::ClientSelectOpponent(oid, word)) => {
                    let mut lock = self.store.write().unwrap();
                    
                    match lock.available_clients.get_mut(&oid)
                                        .filter(|e| e.0 == ClientState::Free) {
                        Some(ol) if oid != self.id => {
                            ol.0 = ClientState::Guesser;
                            let _ = ol.1.send(IMsg::OpponentAssigned(
                                                        self.tx.clone(),
                                                        self.id.clone(),
                                                        word.clone()));
                            self.tx_to_opponent = Some(ol.1.clone());
                            self.client_state = ClientState::Riddlemaker(word);
                            if let Some(me) = lock.available_clients.get_mut(&self.id) {
                                me.0 = self.client_state.clone();
                            };
                            let _ = client_w.write(&Ptcl::OpponentConnectionEstablished(
                                                                oid.clone(),
                                                                self.client_state.clone()));
                        },
                        _ => {
                            let _ = client_w.write(
                                &Ptcl::OpponentConnectionNotEstablished(oid));
                        }
                    }
                },
                (ClientState::Guesser, Ptcl::ClientGuess(word)) => {
                    if let Some(tx) = &self.tx_to_opponent {
                        match &self.opponent_word {
                            Some(w) if *w == word => {
                                let _ = tx.send(IMsg::WordFound);
                                let _ = client_w.write(&Ptcl::ServerWordFound);
                                self.reset_state();
                            },
                            _ => {
                                let _ = tx.send(IMsg::Opponent(Ptcl::ClientGuess(word)));
                            },
                        }
                    }
                },
                (ClientState::Riddlemaker(_), Ptcl::ClientHint(word)) => {
                    if let Some(tx) = &self.tx_to_opponent {
                        let _ = tx.send(IMsg::Opponent(Ptcl::ClientHint(word)));
                    }
                },
                // Logic error: message was valid, but not currently expected
                //   e.g.: Riddlemaker sending a Guess message, or player requesting a new connection while in a game, etc.
                _ => {
                    let _ = client_w.write(&Ptcl::LogicError);
                },
            },
            _ => {},
        }
        Ok(())
    }
    /// State reset, such as when a game was won or forfeited
    fn reset_state(&mut self) {
        self.client_state = ClientState::Free;
        self.tx_to_opponent = None;
        self.opponent_word = None;
        let mut lock = self.store.write().unwrap();
        if let Some(e) = lock.available_clients.get_mut(&self.id) {
            e.0 = ClientState::Free;
        }
    }
}

impl Drop for ServerWorker {
    // Connection lost: clean up
    fn drop(&mut self) {
        let mut lock = self.store.write().unwrap();
        let _ = lock.available_clients.remove(&self.id);
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, mpsc::Receiver, Arc, RwLock};
    use std::collections::{HashMap, VecDeque};
    use crate::ServerState;
    
    struct ServerWorkerMockContext {
        worker: Result<ServerWorker, ()>,
        worker_rx: Receiver<IMsg<Ptcl>>,
        client_r: Box<VecDeque<u8>>,
        client_w: Box<VecDeque<u8>>,
    }
    
    // Create empty store and initialize some number of workers
    macro_rules! initialize {($password: expr, $start_id: expr, $passwords: expr) => {{
        let store = Arc::new(RwLock::new(
            ServerState {
                id_generator: $start_id,
                available_clients: HashMap::new(),
        }));
        let mut workers = Vec::new();
        for p in $passwords {
            let (mut client_r, mut client_w) = (Box::new(VecDeque::<u8>::new()),
                                                Box::new(VecDeque::<u8>::new()));
            let (worker_tx, worker_rx) = mpsc::channel::<IMsg<Ptcl>>();
            let _ = client_r.as_mut().write(&Ptcl::ClientPassword(p.to_string()));
            
            let worker = ServerWorker::new((client_r.as_mut(), client_w.as_mut()),
                                           $password.to_string(),
                                           store.clone(), worker_tx.clone());
            
            workers.push(
                ServerWorkerMockContext {worker, worker_rx, client_r, client_w}
            );
        }
        (store, workers)
    }}}
    
    macro_rules! drain_queues {($w: expr) => {{
        let mut ret = Vec::new();
        for e in $w {
            let mut cw = Vec::new();
            while !e.client_w.is_empty() {
                cw.push(e.client_w.as_mut().read().unwrap());
            }
            ret.push(cw);
        }
        ret
    }}}
    
    macro_rules! assert_queues_empty {($w: expr) => {
        for e in $w {
            assert!(e.client_r.is_empty());
            assert!(e.client_w.is_empty());
            assert!(e.worker_rx.try_recv().is_err());
        }
    }}
    
    fn assert_workers_eq(w: &Vec<ServerWorkerMockContext>,
                         e: Vec<Result<(&str, ClientState, Option<String>), ()>>) {
        assert_eq!(w.len(), e.len());
        for (w, e) in std::iter::zip(w, e) {
            match (&w.worker, e) {
                (Ok(w), Ok((id, state, oword))) => {
                    assert_eq!(w.id, id.to_string());
                    assert_eq!(w.client_state, state);
                    assert_eq!(w.opponent_word, oword);
                },
                (Err(_), Err(_)) => { assert!(true); },
                _ => { assert!(false); },
            }
        }
    }
    
    fn assert_store_eq(s: Store, e: (usize, Vec<(&str, ClientState)>)){
        let lock = s.read().unwrap();
        assert_eq!(lock.id_generator, e.0);
        assert_eq!(lock.available_clients.len(), e.1.len());
        
        for (k, s) in e.1 {
            assert_eq!(lock.available_clients.get(k).unwrap().0, s);
        }
    }
    
    macro_rules! handle_msg {($wc: expr, $msg: expr) => {{
        let wc = $wc;
        match &mut wc.worker {
            Ok(w) => w.handle_message($msg, wc.client_w.as_mut()),
            _ => panic!(),
        }
    }}}
    
    #[test]
    fn test_can_establish_communication() {
        // Execute
        let (store, mut workers) = initialize!("password", 7, vec!["password"]);
        
        // Drain communication queues
        let messages: Vec<Vec<Ptcl>> = drain_queues!(&mut workers);
        
        // Test
        assert_queues_empty!(&workers);
        assert_eq!(messages, vec![vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("7".to_string())]]);
        
        assert_workers_eq(&workers, vec![Ok(("7", ClientState::Free, None))]);
        assert_store_eq(store, (8, vec![("7", ClientState::Free)]));
    }
    
    #[test]
    fn test_rejects_invalid_password() {
        // Execute
        let (store, mut workers) = initialize!("password", 7, vec!["not_password"]);
        
        // Drain communication queues
        let messages: Vec<Vec<Ptcl>> = drain_queues!(&mut workers);
        
        // Test
        assert_queues_empty!(&workers);
        assert_eq!(messages, vec![vec![Ptcl::ServerHello]]);
        assert_workers_eq(&workers, vec![Err(())]);
        assert_store_eq(store, (7, vec![]));
    }
    
    #[test]
    fn test_can_handle_disconnection() {
        // Prepare
        let (store, mut workers) = initialize!("password", 7, vec!["password"]);
        
        // Execute (replacing worker will drop it, removing it from store)
        workers[0].worker = Err(());
        
        // Drain communication queues
        let messages: Vec<Vec<Ptcl>> = drain_queues!(&mut workers);
        
        // Test
        assert_queues_empty!(&workers);
        assert_eq!(messages, vec![vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("7".to_string())]]);
        assert_workers_eq(&workers, vec![Err(())]);
        assert_store_eq(store, (8, vec![]));
    }
    
    #[test]
    fn test_rejects_connection_nonexistent() {
        // Prepare
        let (store, mut workers) = initialize!("password", 7, vec!["password"]);
        
        // Execute
        let r = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                    Ptcl::ClientSelectOpponent("404".to_string(),
                                                               "missing".to_string()))));
        
        // Drain communication queues
        let messages: Vec<Vec<Ptcl>> = drain_queues!(&mut workers);
        
        // Test
        assert_eq!(r, Ok(()));
        assert_queues_empty!(&workers);
        assert_eq!(messages, vec![vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("7".to_string()),
                                       Ptcl::OpponentConnectionNotEstablished("404".to_string())]]);
        assert_workers_eq(&workers, vec![Ok(("7", ClientState::Free, None))]);
        assert_store_eq(store, (8, vec![("7", ClientState::Free)]));
    }
    
    
    #[test]
    fn test_rejects_connection_self() {
        // Prepare
        let (store, mut workers) = initialize!("password", 7, vec!["password"]);
        
        // Execute
        let r = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                        Ptcl::ClientSelectOpponent("7".to_string(),
                                                                   "self".to_string()))));
        
        // Drain communication queues
        let messages: Vec<Vec<Ptcl>> = drain_queues!(&mut workers);
        
        // Test
        assert_eq!(r, Ok(()));
        assert_queues_empty!(&workers);
        assert_eq!(messages, vec![vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("7".to_string()),
                                       Ptcl::OpponentConnectionNotEstablished("7".to_string())]]);
        assert_workers_eq(&workers, vec![Ok(("7", ClientState::Free, None))]);
        assert_store_eq(store, (8, vec![("7", ClientState::Free)]));
    }
    
    #[test]
    fn test_allows_game_start() {
        // Prepare
        let (store, mut workers) = initialize!("password", 7, vec!["password", "password"]);
        
        // Execute
        let r1 = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                        Ptcl::ClientSelectOpponent("8".to_string(),
                                                               "valid".to_string()))));
        let msg2 = workers[1].worker_rx.recv().unwrap();
        let r2 = handle_msg!(&mut workers[1], msg2);
        
        // Drain communication queues
        let messages: Vec<Vec<Ptcl>> = drain_queues!(&mut workers);
        
        // Test
        let state1 = ClientState::Riddlemaker("valid".to_string());
        let state2 = ClientState::Guesser;
        assert_eq!(r1, Ok(()));
        assert_eq!(r2, Ok(()));
        assert_queues_empty!(&workers);
        assert_eq!(messages, vec![vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("7".to_string()),
                                       Ptcl::OpponentConnectionEstablished("8".to_string(), state1.clone())],
                                  vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("8".to_string()),
                                       Ptcl::OpponentConnectionEstablished("7".to_string(), state2.clone())],]);
        assert_workers_eq(&workers, vec![Ok(("7", state1.clone(), None)),
                                         Ok(("8", state2.clone(), Some("valid".to_string())))]);
        assert_store_eq(store, (9, vec![("7", state1.clone()), ("8", state2.clone())]));
    }
    
    #[test]
    fn test_rejects_connection_busy() {
        // Prepare
        let (store, mut workers) = initialize!("password", 7, vec!["password", "password", "password"]);
        
        // Execute
        let r1 = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                        Ptcl::ClientSelectOpponent("8".to_string(),
                                                               "valid".to_string()))));
        let msg2 = workers[1].worker_rx.recv().unwrap();
        let r2 = handle_msg!(&mut workers[1], msg2);
        let r3 = handle_msg!(&mut workers[2], IMsg::Network(Ok(
                                        Ptcl::ClientSelectOpponent("7".to_string(),
                                                               "busy".to_string()))));
        
        // Drain communication queues
        let messages: Vec<Vec<Ptcl>> = drain_queues!(&mut workers);
        
        // Test
        let state1 = ClientState::Riddlemaker("valid".to_string());
        let state2 = ClientState::Guesser;
        let state3 = ClientState::Free;
        assert_eq!(r1, Ok(()));
        assert_eq!(r2, Ok(()));
        assert_eq!(r3, Ok(()));
        assert_queues_empty!(&workers);
        assert_eq!(messages, vec![vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("7".to_string()),
                                       Ptcl::OpponentConnectionEstablished("8".to_string(), state1.clone())],
                                  vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("8".to_string()),
                                       Ptcl::OpponentConnectionEstablished("7".to_string(), state2.clone())],
                                  vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("9".to_string()),
                                       Ptcl::OpponentConnectionNotEstablished("7".to_string())],]);
        assert_workers_eq(&workers, vec![Ok(("7", state1.clone(), None)),
                                         Ok(("8", state2.clone(), Some("valid".to_string()))),
                                         Ok(("9", state3.clone(), None)),]);
        assert_store_eq(store, (10, vec![("7", state1.clone()), ("8", state2.clone()),
                                         ("9", state3.clone())]));
    }
    
    #[test]
    fn test_handles_opponent_disconnection() {
        // Prepare
        let (store, mut workers) = initialize!("password", 7, vec!["password", "password"]);
        
        // Execute
        let r1 = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                        Ptcl::ClientSelectOpponent("8".to_string(),
                                                               "valid".to_string()))));
        let msg2 = workers[1].worker_rx.recv().unwrap();
        let r2 = handle_msg!(&mut workers[1], msg2);
        let r3 = handle_msg!(&mut workers[1], IMsg::Error);
        let msg4 = workers[0].worker_rx.recv().unwrap();
        let r4 = handle_msg!(&mut workers[0], msg4);
        
        // Drain communication queues
        let messages: Vec<Vec<Ptcl>> = drain_queues!(&mut workers);
        
        // Test
        let state1 = ClientState::Riddlemaker("valid".to_string());
        let state2 = ClientState::Guesser;
        let state3 = ClientState::Free;
        assert_eq!(r1, Ok(()));
        assert_eq!(r2, Ok(()));
        assert_eq!(r3, Err(()));
        assert_eq!(r4, Ok(()));
        assert_queues_empty!(&workers);
        assert_eq!(messages, vec![vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("7".to_string()),
                                       Ptcl::OpponentConnectionEstablished("8".to_string(), state1.clone()),
                                       Ptcl::ServerOpponentDisconnected],
                                  vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("8".to_string()),
                                       Ptcl::OpponentConnectionEstablished("7".to_string(), state2.clone())],]);
        assert_workers_eq(&workers, vec![Ok(("7", state3.clone(), None)),
                                         Ok(("8", state2.clone(), Some("valid".to_string()))),]);
        assert_store_eq(store.clone(), (9, vec![("7", state3.clone()), ("8", state2.clone()),]));
        
        // Simulate worker[1] destruction after task terminates
        workers[1].worker = Err(());
        
        // Test
        assert_workers_eq(&workers, vec![Ok(("7", state3.clone(), None)),
                                         Err(()),]);
        assert_store_eq(store, (9, vec![("7", state3.clone()), ]));
    }
    
    #[test]
    fn test_handles_hint() {
        // Prepare
        let (store, mut workers) = initialize!("password", 7, vec!["password", "password"]);
        
        // Execute
        let r1 = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                        Ptcl::ClientSelectOpponent("8".to_string(),
                                                               "valid".to_string()))));
        let msg2 = workers[1].worker_rx.recv().unwrap();
        let r2 = handle_msg!(&mut workers[1], msg2);
        let r3 = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                        Ptcl::ClientHint("correct".to_string()))));
        let msg4 = workers[1].worker_rx.recv().unwrap();
        let r4 = handle_msg!(&mut workers[1], msg4);
        
        // Drain communication queues
        let messages: Vec<Vec<Ptcl>> = drain_queues!(&mut workers);
        
        // Test
        let state1 = ClientState::Riddlemaker("valid".to_string());
        let state2 = ClientState::Guesser;
        assert_eq!(r1, Ok(()));
        assert_eq!(r2, Ok(()));
        assert_eq!(r3, Ok(()));
        assert_eq!(r4, Ok(()));
        assert_queues_empty!(&workers);
        assert_eq!(messages, vec![vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("7".to_string()),
                                       Ptcl::OpponentConnectionEstablished("8".to_string(), state1.clone()),],
                                  vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("8".to_string()),
                                       Ptcl::OpponentConnectionEstablished("7".to_string(), state2.clone()),
                                       Ptcl::ClientHint("correct".to_string())],]);
        assert_workers_eq(&workers, vec![Ok(("7", state1.clone(), None)),
                                         Ok(("8", state2.clone(), Some("valid".to_string()))),]);
        assert_store_eq(store, (9, vec![("7", state1.clone()), ("8", state2.clone()),]));
    }
    
    #[test]
    fn test_handles_incorrect_guess() {
        // Prepare
        let (store, mut workers) = initialize!("password", 7, vec!["password", "password"]);
        
        // Execute
        let r1 = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                        Ptcl::ClientSelectOpponent("8".to_string(),
                                                               "valid".to_string()))));
        let msg2 = workers[1].worker_rx.recv().unwrap();
        let r2 = handle_msg!(&mut workers[1], msg2);
        let r3 = handle_msg!(&mut workers[1], IMsg::Network(Ok(
                                        Ptcl::ClientGuess("incorrect".to_string()))));
        let msg4 = workers[0].worker_rx.recv().unwrap();
        let r4 = handle_msg!(&mut workers[0], msg4);
        
        // Drain communication queues
        let messages: Vec<Vec<Ptcl>> = drain_queues!(&mut workers);
        
        // Test
        let state1 = ClientState::Riddlemaker("valid".to_string());
        let state2 = ClientState::Guesser;
        assert_eq!(r1, Ok(()));
        assert_eq!(r2, Ok(()));
        assert_eq!(r3, Ok(()));
        assert_eq!(r4, Ok(()));
        assert_queues_empty!(&workers);
        assert_eq!(messages, vec![vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("7".to_string()),
                                       Ptcl::OpponentConnectionEstablished("8".to_string(), state1.clone()),
                                       Ptcl::ClientGuess("incorrect".to_string())],
                                  vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("8".to_string()),
                                       Ptcl::OpponentConnectionEstablished("7".to_string(), state2.clone())],]);
        assert_workers_eq(&workers, vec![Ok(("7", state1.clone(), None)),
                                         Ok(("8", state2.clone(), Some("valid".to_string()))),]);
        assert_store_eq(store, (9, vec![("7", state1.clone()), ("8", state2.clone()),]));
    }
    
    #[test]
    fn test_handles_correct_guess() {
        // Prepare
        let (store, mut workers) = initialize!("password", 7, vec!["password", "password"]);
        
        // Execute
        let r1 = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                        Ptcl::ClientSelectOpponent("8".to_string(),
                                                               "valid".to_string()))));
        let msg2 = workers[1].worker_rx.recv().unwrap();
        let r2 = handle_msg!(&mut workers[1], msg2);
        let r3 = handle_msg!(&mut workers[1], IMsg::Network(Ok(
                                        Ptcl::ClientGuess("valid".to_string()))));
        let msg4 = workers[0].worker_rx.recv().unwrap();
        let r4 = handle_msg!(&mut workers[0], msg4);
        
        // Drain communication queues
        let messages: Vec<Vec<Ptcl>> = drain_queues!(&mut workers);
        
        // Test
        let state1 = ClientState::Riddlemaker("valid".to_string());
        let state2 = ClientState::Guesser;
        let state3 = ClientState::Free;
        assert_eq!(r1, Ok(()));
        assert_eq!(r2, Ok(()));
        assert_eq!(r3, Ok(()));
        assert_eq!(r4, Ok(()));
        assert_queues_empty!(&workers);
        assert_eq!(messages, vec![vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("7".to_string()),
                                       Ptcl::OpponentConnectionEstablished("8".to_string(), state1.clone()),
                                       Ptcl::ServerWordFound],
                                  vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("8".to_string()),
                                       Ptcl::OpponentConnectionEstablished("7".to_string(), state2.clone()),
                                       Ptcl::ServerWordFound],]);
        assert_workers_eq(&workers, vec![Ok(("7", state3.clone(), None)),
                                         Ok(("8", state3.clone(), None)),]);
        assert_store_eq(store, (9, vec![("7", state3.clone()), ("8", state3.clone()),]));
    }
    
    #[test]
    fn test_handles_riddlemaker_guess() {
        // Prepare
        let (store, mut workers) = initialize!("password", 7, vec!["password", "password"]);
        
        // Execute
        let r1 = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                        Ptcl::ClientSelectOpponent("8".to_string(),
                                                               "valid".to_string()))));
        let msg2 = workers[1].worker_rx.recv().unwrap();
        let r2 = handle_msg!(&mut workers[1], msg2);
        let r3 = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                        Ptcl::ClientGuess("valid".to_string()))));
        
        // Drain communication queues
        let messages: Vec<Vec<Ptcl>> = drain_queues!(&mut workers);
        
        // Test
        let state1 = ClientState::Riddlemaker("valid".to_string());
        let state2 = ClientState::Guesser;
        assert_eq!(r1, Ok(()));
        assert_eq!(r2, Ok(()));
        assert_eq!(r3, Ok(()));
        assert_queues_empty!(&workers);
        assert_eq!(messages, vec![vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("7".to_string()),
                                       Ptcl::OpponentConnectionEstablished("8".to_string(), state1.clone()),
                                       Ptcl::LogicError],
                                  vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("8".to_string()),
                                       Ptcl::OpponentConnectionEstablished("7".to_string(), state2.clone())],]);
        assert_workers_eq(&workers, vec![Ok(("7", state1.clone(), None)),
                                         Ok(("8", state2.clone(), Some("valid".to_string()))),]);
        assert_store_eq(store, (9, vec![("7", state1.clone()), ("8", state2.clone()),]));
    }
    
    #[test]
    fn test_handles_connection_after_game_ends() {
        // Prepare
        let (store, mut workers) = initialize!("password", 7, vec!["password", "password"]);
        
        // Execute
        let r1 = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                        Ptcl::ClientSelectOpponent("8".to_string(),
                                                               "valid".to_string()))));
        let msg2 = workers[1].worker_rx.recv().unwrap();
        let r2 = handle_msg!(&mut workers[1], msg2);
        let r3 = handle_msg!(&mut workers[1], IMsg::Network(Ok(
                                        Ptcl::ClientGuess("valid".to_string()))));
        let msg4 = workers[0].worker_rx.recv().unwrap();
        let r4 = handle_msg!(&mut workers[0], msg4);
        let r5 = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                        Ptcl::ClientSelectOpponent("8".to_string(),
                                                               "valid".to_string()))));
        let msg6 = workers[1].worker_rx.recv().unwrap();
        let r6 = handle_msg!(&mut workers[1], msg6);
        
        // Drain communication queues
        let messages: Vec<Vec<Ptcl>> = drain_queues!(&mut workers);
        
        // Test
        let state1 = ClientState::Riddlemaker("valid".to_string());
        let state2 = ClientState::Guesser;
        assert_eq!(r1, Ok(()));
        assert_eq!(r2, Ok(()));
        assert_eq!(r3, Ok(()));
        assert_eq!(r4, Ok(()));
        assert_eq!(r5, Ok(()));
        assert_eq!(r6, Ok(()));
        assert_queues_empty!(&workers);
        assert_eq!(messages, vec![vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("7".to_string()),
                                       Ptcl::OpponentConnectionEstablished("8".to_string(), state1.clone()),
                                       Ptcl::ServerWordFound,
                                       Ptcl::OpponentConnectionEstablished("8".to_string(), state1.clone())],
                                  vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("8".to_string()),
                                       Ptcl::OpponentConnectionEstablished("7".to_string(), state2.clone()),
                                       Ptcl::ServerWordFound,
                                       Ptcl::OpponentConnectionEstablished("7".to_string(), state2.clone()),],]);
        assert_workers_eq(&workers, vec![Ok(("7", state1.clone(), None)),
                                         Ok(("8", state2.clone(), Some("valid".to_string()))),]);
        assert_store_eq(store, (9, vec![("7", state1.clone()), ("8", state2.clone()),]));
    }
    
    #[test]
    fn test_handles_connection_before_game_ends() {
        // Prepare
        let (store, mut workers) = initialize!("password", 7, vec!["password", "password", "password"]);
        
        // Execute
        let r1 = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                        Ptcl::ClientSelectOpponent("8".to_string(),
                                                               "valid".to_string()))));
        let msg2 = workers[1].worker_rx.recv().unwrap();
        let r2 = handle_msg!(&mut workers[1], msg2);
        let r3 = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                        Ptcl::ClientSelectOpponent("9".to_string(),
                                                               "valid".to_string()))));
        
        // Drain communication queues
        let messages: Vec<Vec<Ptcl>> = drain_queues!(&mut workers);
        
        // Test
        let state1 = ClientState::Riddlemaker("valid".to_string());
        let state2 = ClientState::Guesser;
        let state3 = ClientState::Free;
        assert_eq!(r1, Ok(()));
        assert_eq!(r2, Ok(()));
        assert_eq!(r3, Ok(()));
        //assert_queues_empty!(&workers);
        assert_eq!(messages, vec![vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("7".to_string()),
                                       Ptcl::OpponentConnectionEstablished("8".to_string(), state1.clone()),
                                       Ptcl::LogicError],
                                  vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("8".to_string()),
                                       Ptcl::OpponentConnectionEstablished("7".to_string(), state2.clone()),],
                                  vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("9".to_string()),],]);
        assert_workers_eq(&workers, vec![Ok(("7", state1.clone(), None)),
                                         Ok(("8", state2.clone(), Some("valid".to_string()))),
                                         Ok(("9", state3.clone(), None))]);
        assert_store_eq(store, (10, vec![("7", state1.clone()), ("8", state2.clone()),
                                         ("9", state3.clone()),]));
    }
    
    #[test]
    fn test_handles_guess_without_opponent() {
        // Prepare
        let (store, mut workers) = initialize!("password", 7, vec!["password"]);
        
        // Execute
        let r = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                        Ptcl::ClientGuess("alone".to_string()))));
        
        // Drain communication queues
        let messages: Vec<Vec<Ptcl>> = drain_queues!(&mut workers);
        
        // Test
        assert_eq!(r, Ok(()));
        assert_queues_empty!(&workers);
        assert_eq!(messages, vec![vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("7".to_string()),
                                       Ptcl::LogicError]]);
        assert_workers_eq(&workers, vec![Ok(("7", ClientState::Free, None))]);
        assert_store_eq(store, (8, vec![("7", ClientState::Free)]));
    }
    
    #[test]
    fn test_handles_hint_without_opponent() {
        // Prepare
        let (store, mut workers) = initialize!("password", 7, vec!["password"]);
        
        // Execute
        let r = handle_msg!(&mut workers[0], IMsg::Network(Ok(
                                        Ptcl::ClientHint("alone".to_string()))));
        
        // Drain communication queues
        let messages: Vec<Vec<Ptcl>> = drain_queues!(&mut workers);
        
        // Test
        assert_eq!(r, Ok(()));
        assert_queues_empty!(&workers);
        assert_eq!(messages, vec![vec![Ptcl::ServerHello,
                                       Ptcl::ConnectionEstablished("7".to_string()),
                                       Ptcl::LogicError]]);
        assert_workers_eq(&workers, vec![Ok(("7", ClientState::Free, None))]);
        assert_store_eq(store, (8, vec![("7", ClientState::Free)]));
    }
    
}


/// Find named argument, parse it to T
pub fn parse_arg<T: std::str::FromStr>(
    name: &str,
    args: &[String],
    options: &[&str],
) -> Option<T> {
    let mut it = args.iter();
    while let Some(value) = it.next() {
        if value == name {
            return it.next().and_then(|e| e.parse::<T>().ok());
        } else if options.iter().any(|e| *e == value) {
            it.next();
        }
    }
    None
}

/// Macro to generate associated function to spawn a task to block on network reader read
/// Sends Self::Network(Ok(a)) when message is read,
///       Self::Network(Err(())) when message deserialization fails,
///   and Self::Error on unrecoverable error (such as lost connection).
macro_rules! impl_spawn_network_listener {($vis: vis) => {
$vis fn spawn_network_listener<R>(
    reader: Box<R>,
    transmitter: Sender<Self>,
) -> JoinHandle<()>
    where R: ProtocolReader<T> + Send + ?Sized + 'static,
          T: for<'a> Deserialize<'a> + Send + 'static
{
    spawn_blocking(move || { loop {
        match reader.read() {
            Ok(a) => {
                transmitter.send(Self::Network(Ok(a))).unwrap();
            },
            Err(e) => match *e {
                ErrorKind::Io(_) => {
                    transmitter.send(Self::Error).unwrap();
                    break;
                },
                _ => {
                    transmitter.send(Self::Network(Err(()))).unwrap();
                },
            }
        }
    }})
}
}}
pub(crate) use impl_spawn_network_listener;


#[cfg(test)]
mod tests {
    use crate::parse_arg;
    
    fn arg_set_1() -> Vec<String> {
        vec!["--received-option".to_string(), "value1".to_string(),
             "--different-received-option".to_string(), "value2".to_string(),
             "--last-received-option".to_string(), "value3".to_string()]
    }
    fn arg_set_2() -> Vec<String> {
        vec!["--received-option".to_string(), "--different-received-option".to_string(),
             "--different-received-option".to_string(), "value2".to_string(),
             "--last-received-option".to_string(), "value3".to_string()]
    }
    fn empty() -> Vec<&'static str> { vec![] }
    fn accepted_options() -> Vec<&'static str> {
        vec!["--received-option", "--different-received-option", "--last-received-option"]
    }
    
    #[test]
    fn test_doesnt_find_missing() {
        assert_eq!(parse_arg::<String>("--searched-option", &arg_set_1(), &empty()),
                   None);
    }
    
    #[test]
    fn test_finds_only() {
        let args = vec!["--received-option".to_string(), "value".to_string()];
        
        assert_eq!(parse_arg::<String>("--received-option", &args, &empty()),
                   Some("value".to_string()));
    }
    
    #[test]
    fn test_finds_first() {
        assert_eq!(parse_arg::<String>("--received-option", &arg_set_1(), &empty()),
                   Some("value1".to_string()));
    }
    
    #[test]
    fn test_finds_mid() {
        assert_eq!(parse_arg::<String>("--different-received-option", &arg_set_1(), &empty()),
                   Some("value2".to_string()));
    }
    
    #[test]
    fn test_finds_last() {
        assert_eq!(parse_arg::<String>("--last-received-option", &arg_set_1(), &empty()),
                   Some("value3".to_string()));
    }
    
    #[test]
    fn test_finds_tricky() {
        assert_eq!(parse_arg::<String>("--different-received-option",
                                       &arg_set_2(), &accepted_options()),
                   Some("value2".to_string()));
    }
}

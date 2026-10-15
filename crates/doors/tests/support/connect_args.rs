use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Args {
    Socket(PathBuf),
    Hub {
        hub: Option<String>,
        fiber: Option<String>,
    },
}

pub(crate) fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, ()> {
    let mut args = args.into_iter().peekable();
    let first = args.peek().ok_or(())?;
    if !first.starts_with('-') {
        let path = args.next().ok_or(())?;
        return if args.next().is_none() {
            Ok(Args::Socket(path.into()))
        } else {
            Err(())
        };
    }
    let mut hub = None;
    let mut fiber = None;
    while let Some(flag) = args.next() {
        let place = match flag.as_str() {
            "--hub" => &mut hub,
            "--fiber" => &mut fiber,
            _ => return Err(()),
        };
        if place.is_some() {
            return Err(());
        }
        let value = args
            .next()
            .filter(|value| !value.starts_with('-'))
            .ok_or(())?;
        *place = Some(value);
    }
    Ok(Args::Hub { hub, fiber })
}

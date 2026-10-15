//! The connect jig's socket and named-hub argument forms.
#[path = "support/connect_args.rs"]
mod connect_args;
use connect_args::{Args, parse};

#[test]
fn socket_and_hub_forms_keep_their_values_in_either_order() {
    for (args, expected) in [
        (vec!["/tmp/socket"], Args::Socket("/tmp/socket".into())),
        (
            vec!["--hub", "box"],
            Args::Hub {
                hub: Some("box".into()),
                fiber: None,
            },
        ),
        (
            vec!["--fiber", "/tmp/fiber"],
            Args::Hub {
                hub: None,
                fiber: Some("/tmp/fiber".into()),
            },
        ),
        (
            vec!["--hub", "box", "--fiber", "/tmp/fiber"],
            Args::Hub {
                hub: Some("box".into()),
                fiber: Some("/tmp/fiber".into()),
            },
        ),
        (
            vec!["--fiber", "/tmp/fiber", "--hub", "box"],
            Args::Hub {
                hub: Some("box".into()),
                fiber: Some("/tmp/fiber".into()),
            },
        ),
    ] {
        assert_eq!(
            parse(args.into_iter().map(str::to_owned)).unwrap(),
            expected
        );
    }
}

#[test]
fn absent_values_unknown_flags_and_mixed_forms_are_usage_errors() {
    for args in [
        vec![],
        vec!["--hub"],
        vec!["--fiber"],
        vec!["--unknown"],
        vec!["--hub", "--fiber", "bin"],
        vec!["socket", "extra"],
        vec!["socket", "--hub", "box"],
        vec!["--hub", "box", "--hub", "other"],
    ] {
        assert!(parse(args.into_iter().map(str::to_owned)).is_err());
    }
}

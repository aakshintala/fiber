use super::*;

fn tickets(body: &str) -> Vec<String> {
    ticket(body).into_iter().collect()
}

#[test]
fn the_ticket_is_the_resolved_issue() {
    assert_eq!(
        tickets("Does a thing.\n\nResolves #212\n"),
        vec!["212".to_owned()]
    );
    assert_eq!(tickets("fixes #7"), vec!["7".to_owned()]);
    assert_eq!(tickets("Closed #9."), vec!["9".to_owned()]);
    assert_eq!(tickets("See #3, then close\n#4"), vec!["4".to_owned()]);
}

#[test]
fn the_ticket_lists_every_resolved_issue_in_order() {
    assert_eq!(
        tickets("Resolves #365\nResolves #384\n"),
        vec!["365".to_owned(), "384".to_owned()]
    );
    assert_eq!(
        tickets("Fixes #1, closes #2 and resolves #3"),
        vec!["1".to_owned(), "2".to_owned(), "3".to_owned()]
    );
}

#[test]
fn there_is_no_ticket_without_a_closing_keyword() {
    for body in [
        "See #212",
        "",
        "Resolves#212",
        "unresolves #5",
        "Resolves #",
        "Resolves #12a",
        "Resolves 12",
    ] {
        assert_eq!(tickets(body), Vec::<String>::new(), "{body:?}");
    }
}

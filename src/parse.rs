//! The grammars of the strings the fake reads out of requests, of git and of its own state.
//!
//! What a standard defines is read with the crate that defines it: URLs with [`url`], the
//! `Authorization` header with [`headers`], and query strings and form bodies with
//! [`url::form_urlencoded`]. What is left is the fake's own little grammars, parsed here with
//! [`winnow`]: a credential, a token scope, a git ref, a repository key, a colour, a label
//! list and a repository name.

use winnow::ModalParser;
use winnow::combinator::{alt, eof, opt, separated};
use winnow::error::ContextError;
use winnow::prelude::*;
use winnow::token::{literal, take_till, take_while};

/// Longest repository name Forgejo accepts, in bytes.
const NAME_LIMIT: usize = 100;

/// The scheme of a credential and the credentials that follow it, as RFC 7235 spells them: a
/// scheme, one space, and then whatever the scheme takes, spaces trimmed off. `None` for a
/// value that is no credential at all.
pub fn credentials(value: &str) -> Option<(&str, &str)> {
    let mut input = value;
    let scheme = take(
        take_while(1.., |c: char| !c.is_ascii_whitespace()),
        &mut input,
    )?;
    take(' ', &mut input)?;
    let rest = take(take_while(0.., |_| true), &mut input)?;
    Some((scheme, rest.trim()))
}

/// The verb of a token scope and the resource it names, as the spec spells its scopes:
/// `read:issue` and `write:issue`, the resource being a word of its own. `None` for anything
/// else, which is no scope.
pub fn scope(scope: &str) -> Option<(&str, &str)> {
    let mut input = scope;
    let verb = take(alt((literal("read"), literal("write"))), &mut input)?;
    take(':', &mut input)?;
    let resource = take(
        take_while(1.., |c: char| c.is_ascii_alphanumeric()),
        &mut input,
    )?;
    take(eof.void(), &mut input)?;
    Some((verb, resource))
}

/// The branch a git ref names, which is what every ref under `refs/heads/` holds and what no
/// other ref does; a tag names no branch.
pub fn branch_of(reference: &str) -> Option<&str> {
    let mut input = reference;
    take(literal("refs/heads/").void(), &mut input)?;
    take(take_while(0.., |_| true), &mut input)
}

/// The owner and the name a repository key is made of, split at its first `/`.
pub fn owner_and_name(key: &str) -> Option<(&str, &str)> {
    let mut input = key;
    let owner = take(take_till(0.., '/'), &mut input)?;
    take('/', &mut input)?;
    let name = take(take_while(0.., |_| true), &mut input)?;
    Some((owner, name))
}

/// The labels a `labels` query parameter lists, lowercased and trimmed, with the empty ones
/// dropped, so `?labels=bug,%20docs,` lists the two labels a client may write.
pub fn label_names(listed: &str) -> Vec<String> {
    let mut input = listed;
    let names: Vec<&str> =
        take(separated(1.., take_till(0.., ','), ','), &mut input).unwrap_or_default();
    names
        .into_iter()
        .map(|name| name.trim().to_lowercase())
        .filter(|name| !name.is_empty())
        .collect()
}

/// A colour as Forgejo stores it: three or six hexadecimal digits, lowercase, with the `#`
/// clients may write dropped. A colour that is not one is `Err` with the digits as the client
/// wrote them, which is what the fake names in its error.
pub fn color(written: &str) -> Result<String, String> {
    let mut input = written;
    // Every character behind the `#` is a digit of the colour, whether they are digits or not.
    let Some(digits) = after_hash(&mut input) else {
        return Err(written.to_lowercase());
    };
    match alt((hex_digits(6), hex_digits(3))).parse(digits) {
        Ok(digits) => Ok(digits.to_ascii_lowercase()),
        Err(_) => Err(digits.to_lowercase()),
    }
}

/// Everything a client wrote after the `#` of a colour, which it may write and may not.
fn after_hash<'i>(input: &mut &'i str) -> Option<&'i str> {
    take(opt('#').void(), input)?;
    take(take_while(0.., |_| true), input)
}

/// Exactly `count` hexadecimal digits and nothing else.
fn hex_digits<'i>(count: usize) -> impl ModalParser<&'i str, &'i str, ContextError> {
    (
        take_while(count..=count, |c: char| c.is_ascii_hexdigit()),
        eof.void(),
    )
        .map(|(digits, ())| digits)
}

/// Whether `name` is a repository name as Forgejo accepts it: at most [`NAME_LIMIT`] bytes of
/// letters, digits, `-`, `_` and `.`, and neither `.` nor `..` alone.
pub fn repository_name(name: &str) -> bool {
    fn character(c: char) -> bool {
        c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
    }

    let mut input = name;
    take(take_while(1.., character), &mut input).is_some_and(|_| {
        input.is_empty() && name.len() <= NAME_LIMIT && !matches!(name, "." | "..")
    })
}

/// Runs one grammar `parser` over the front of `input` and leaves the rest of it in place. Most
/// of these grammars are a prefix and what follows it, so each takes its parts one after another
/// rather than a whole string at once; a failure to match is a plain `None`.
fn take<'i, O>(
    mut parser: impl ModalParser<&'i str, O, ContextError>,
    input: &mut &'i str,
) -> Option<O> {
    parser.parse_next(input).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_credential_is_a_scheme_one_space_and_its_credentials() {
        assert_eq!(credentials("token fjo_1"), Some(("token", "fjo_1")));
        assert_eq!(credentials("Bearer fjo_1"), Some(("Bearer", "fjo_1")));
        assert_eq!(credentials("Basic  fjo_1  "), Some(("Basic", "fjo_1")));
        assert_eq!(
            credentials("token fjo_1 more"),
            Some(("token", "fjo_1 more"))
        );
        assert_eq!(credentials("token"), None);
        assert_eq!(credentials(""), None);
    }

    #[test]
    fn a_scope_is_a_read_or_a_write_verb_and_a_resource() {
        assert_eq!(scope("read:issue"), Some(("read", "issue")));
        assert_eq!(scope("write:repository"), Some(("write", "repository")));
        assert_eq!(scope("read:"), None);
        assert_eq!(scope("admin:issue"), None);
        assert_eq!(scope("read:issue:extra"), None);
        assert_eq!(scope("repository"), None);
    }

    #[test]
    fn only_a_ref_under_refs_heads_names_a_branch() {
        assert_eq!(branch_of("refs/heads/main"), Some("main"));
        assert_eq!(branch_of("refs/heads/topic/deep"), Some("topic/deep"));
        assert_eq!(branch_of("refs/heads/"), Some(""));
        assert_eq!(branch_of("refs/tags/v1"), None);
        assert_eq!(branch_of("main"), None);
    }

    #[test]
    fn a_repository_key_is_an_owner_and_a_name() {
        assert_eq!(owner_and_name("alice/tool"), Some(("alice", "tool")));
        assert_eq!(
            owner_and_name("alice/tool/extra"),
            Some(("alice", "tool/extra"))
        );
        assert_eq!(owner_and_name("alice"), None);
        assert_eq!(owner_and_name(""), None);
    }

    #[test]
    fn labels_are_listed_lowercased_and_trimmed() {
        assert_eq!(label_names("bug, docs"), ["bug", "docs"]);
        assert_eq!(label_names("Bug, ,docs,"), ["bug", "docs"]);
        assert_eq!(label_names(""), Vec::<String>::new());
    }

    #[test]
    fn a_colour_is_three_or_six_hexadecimal_digits() {
        assert_eq!(color("ededed"), Ok("ededed".to_owned()));
        assert_eq!(color("#ABC"), Ok("abc".to_owned()));
        assert_eq!(color("abc"), Ok("abc".to_owned()));
        assert_eq!(color("#abcdeff"), Err("abcdeff".to_owned()));
        assert_eq!(color("#ABCD"), Err("abcd".to_owned()));
        assert_eq!(color(""), Err(String::new()));
        assert_eq!(color("rebeccapurple"), Err("rebeccapurple".to_owned()));
    }

    #[test]
    fn a_repository_name_is_ascii_of_at_most_a_hundred_bytes() {
        assert!(repository_name("tool"));
        assert!(repository_name("tool.git"));
        assert!(repository_name("a.b-c_d"));
        assert!(repository_name(&"a".repeat(NAME_LIMIT)));
        assert!(!repository_name(&"a".repeat(NAME_LIMIT + 1)));
        assert!(!repository_name(""));
        assert!(!repository_name("."));
        assert!(!repository_name(".."));
        assert!(!repository_name("a b"));
    }
}

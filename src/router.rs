//! Matches request paths against the generated operation table, with
//! [`ninot_common::router`].

pub(crate) use ninot_common::router::Router;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::{Method, OPERATIONS};
    use winnow::ModalResult;
    use winnow::combinator::{alt, repeat};
    use winnow::prelude::*;
    use winnow::token::{take_till, take_while};

    /// Fills every parameter of `template` with `x`, as a request to that operation would: the
    /// text before a `{parameter}` is kept and the parameter itself is replaced.
    fn concrete(template: &str) -> String {
        /// The text before a `{parameter}`, and the `x` that fills it.
        fn parameter(input: &mut &str) -> ModalResult<String> {
            let mut filled = take_till(1.., '{').parse_next(input)?.to_owned();
            '{'.parse_next(input)?;
            take_till(1.., '}').void().parse_next(input)?;
            '}'.parse_next(input)?;
            filled.push('x');
            Ok(filled)
        }

        /// The literal text between two parameters.
        fn literal_text(input: &mut &str) -> ModalResult<String> {
            take_while(1.., |c: char| c != '{')
                .parse_next(input)
                .map(str::to_owned)
        }

        let mut input = template;
        repeat(0.., alt((parameter, literal_text)))
            .parse_next(&mut input)
            .unwrap_or_default()
    }

    #[test]
    fn every_operation_of_the_pinned_spec_routes_to_itself() {
        let router = Router::new(OPERATIONS);
        for info in OPERATIONS {
            let path = concrete(info.path);
            let found = router.find(info.method, &path).unwrap();
            assert_eq!(
                found.info.path,
                info.path,
                "{} {} routed to {}",
                info.method.as_str(),
                info.path,
                found.info.id
            );
        }
    }

    #[test]
    fn a_segment_mixing_a_parameter_and_literal_text_routes_to_itself() {
        let router = Router::new(OPERATIONS);
        let diff = router
            .find(Method::Get, "/api/v1/repos/o/r/git/commits/abc123.diff")
            .unwrap();
        assert_eq!(diff.info.id, "repoDownloadCommitDiffOrPatch");
        assert_eq!(diff.params["sha"], "abc123");
        assert_eq!(diff.params["diffType"], "diff");
        let sha = router
            .find(Method::Get, "/api/v1/repos/o/r/git/commits/abc123")
            .unwrap();
        assert_eq!(sha.info.id, "repoGetSingleCommit");
        assert_eq!(sha.params["sha"], "abc123");
    }

    #[test]
    fn a_filepath_keeps_its_slashes() {
        let router = Router::new(OPERATIONS);
        let found = router
            .find(Method::Get, "/api/v1/repos/o/r/contents/dir/README.md")
            .unwrap();
        assert_eq!(found.info.id, "repoGetContents");
        assert_eq!(found.params["filepath"], "dir/README.md");
    }
}

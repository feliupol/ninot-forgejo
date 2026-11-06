//! Request and response types shared by all handlers.
//!
//! Handlers deserialize request bodies into generated input models and answer with generated
//! response models, which [`ApiResponse`] serializes as they leave.

use std::collections::HashMap;

use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use crate::parse;
use crate::types::ApiError;

/// The caller a request is authenticated as.
#[derive(Clone, Debug)]
pub struct Caller {
    pub login: String,
    pub scopes: Vec<String>,
}

impl Caller {
    /// Whether the caller holds `scope`. Forgejo's `write:` scopes also grant the matching
    /// `read:` scope, so a token that may write to repositories may also read them.
    pub fn has_scope(&self, scope: &str) -> bool {
        if self.granted(scope) {
            return true;
        }
        match parse::scope(scope) {
            Some(("read", resource)) => self.granted(&format!("write:{resource}")),
            _ => false,
        }
    }

    fn granted(&self, scope: &str) -> bool {
        self.scopes.iter().any(|held| held == scope)
    }
}

/// One API request, already routed to an operation.
pub struct ApiRequest {
    /// Request path without the `/api/v1` prefix, such as `/repos/o/r`.
    pub path: String,
    /// Path parameters by their spec name.
    pub params: HashMap<String, String>,
    pub query: Vec<(String, String)>,
    pub body: bytes::Bytes,
    pub caller: Option<Caller>,
    /// Base URL of the API as the client addressed it, such as `http://127.0.0.1:1234/api/v1`.
    pub api_url: String,
    /// Base URL of the server, used for web and git URLs.
    pub web_url: String,
}

/// A typed JSON field whose absence and explicit null have different PATCH meanings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PatchField<T> {
    /// The request omitted this field.
    #[default]
    Missing,
    /// The request supplied this field as JSON `null`.
    Null,
    /// The request supplied a non-null value.
    Value(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for PatchField<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<T>::deserialize(deserializer).map(|value| match value {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

impl ApiRequest {
    /// A path parameter the route guarantees.
    pub fn param(&self, name: &str) -> &str {
        self.params
            .get(name)
            .unwrap_or_else(|| panic!("route has no parameter {name}"))
    }

    /// A query parameter's value, or `None`.
    pub fn query(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Deserializes the JSON body into the operation's input model, or returns a 400 response
    /// when the body is invalid. An absent body is the empty object.
    pub fn json_as<T: DeserializeOwned>(&self) -> Result<T, ApiResponse> {
        self.json_as_with_status(400)
    }

    /// Deserializes a request model, keeping malformed JSON at 400 while assigning typed model
    /// validation failures the operation's status code.
    pub fn json_as_with_status<T: DeserializeOwned>(
        &self,
        model_error_status: u16,
    ) -> Result<T, ApiResponse> {
        if self.body.is_empty() {
            return serde_json::from_value(Value::Object(Map::new()))
                .map_err(|_| ApiResponse::error(model_error_status, "Problems parsing JSON"));
        }
        serde_json::from_slice(&self.body).map_err(|error| {
            let status = match error.classify() {
                serde_json::error::Category::Syntax | serde_json::error::Category::Eof => 400,
                serde_json::error::Category::Data | serde_json::error::Category::Io => {
                    model_error_status
                }
            };
            ApiResponse::error(status, "Problems parsing JSON")
        })
    }

    /// The authenticated caller, or a 401 response.
    pub fn require_caller(&self) -> Result<&Caller, ApiResponse> {
        self.caller.as_ref().ok_or_else(ApiResponse::unauthorized)
    }

    /// The caller, or a 401; with a token lacking `scope`, a 403 naming it.
    pub fn require_scope(&self, scope: &str) -> Result<&Caller, ApiResponse> {
        let caller = self.require_caller()?;
        if caller.has_scope(scope) {
            Ok(caller)
        } else {
            Err(ApiResponse::error(
                403,
                &format!("token does not have at least one of required scopes: {scope}"),
            ))
        }
    }
}

/// Body of an API response.
#[derive(Debug)]
pub enum Body {
    Empty,
    Json(Value),
    Bytes(Vec<u8>),
}

/// One API response.
#[derive(Debug)]
pub struct ApiResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl ApiResponse {
    /// A response whose body is `body` serialized, which is the generated model of what the
    /// spec declares the operation answers with.
    pub fn json<T: Serialize>(status: u16, body: T) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Body::Json(
                serde_json::to_value(body).expect("a response body always serializes"),
            ),
        }
    }

    pub fn ok<T: Serialize>(body: T) -> Self {
        Self::json(200, body)
    }

    /// The spec's `APIEmpty`: a 200 whose body is an empty JSON object, which is what Forgejo
    /// answers a merge with.
    pub fn ok_empty() -> Self {
        Self::json(200, Map::<String, Value>::new())
    }

    pub fn no_content() -> Self {
        Self {
            status: 204,
            headers: Vec::new(),
            body: Body::Empty,
        }
    }

    pub fn bytes(status: u16, content_type: &str, bytes: Vec<u8>) -> Self {
        Self {
            status,
            headers: vec![("content-type".to_owned(), content_type.to_owned())],
            body: Body::Bytes(bytes),
        }
    }

    /// A Forgejo-shaped error body. Forgejo names the API URL of the request in it, which
    /// handlers do not know; the server fills in the `null` left here.
    pub fn error(status: u16, message: &str) -> Self {
        Self::json(
            status,
            ApiError {
                message: message.to_owned(),
                url: None,
            },
        )
    }

    pub fn not_found() -> Self {
        Self::error(404, "Not Found")
    }

    pub fn unauthorized() -> Self {
        Self::error(401, "token is required")
    }

    pub fn with_header(mut self, name: &str, value: String) -> Self {
        self.headers.push((name.to_owned(), value));
        self
    }
}

/// Result type of handlers: an error is itself a response.
pub type ApiResult = Result<ApiResponse, ApiResponse>;

/// One page of `items`, as Forgejo's `page` and `limit` query parameters select, with the
/// `X-Total-Count` and `Link` headers Forgejo answers a paged list with.
pub fn paginate<T: Serialize>(request: &ApiRequest, items: Vec<T>) -> ApiResponse {
    let limit = limit_of(request);
    let page = page_of(request);
    let total = items.len();
    let body: Vec<T> = items
        .into_iter()
        .skip((page - 1).saturating_mul(limit))
        .take(limit)
        .collect();
    let response = ApiResponse::ok(body).with_header("x-total-count", total.to_string());
    let pages = total.div_ceil(limit);
    if pages > 1 {
        response.with_header("link", link(request, page, pages))
    } else {
        response
    }
}

/// The number of items of a page, `limit` as the request asks for it; 0 means the default.
fn limit_of(request: &ApiRequest) -> usize {
    request
        .query("limit")
        .and_then(|limit| limit.parse::<usize>().ok())
        .filter(|&limit| limit > 0)
        .unwrap_or(PAGINATION_DEFAULT)
}

/// The number of the page the request asks for, counted from one.
fn page_of(request: &ApiRequest) -> usize {
    request
        .query("page")
        .and_then(|page| page.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1)
}

/// The `Link` header of a page out of `pages`: the pages around it, as Forgejo names them.
fn link(request: &ApiRequest, page: usize, pages: usize) -> String {
    let first = (page > 1).then_some(1);
    let last = (page < pages).then_some(pages);
    [
        ("prev", (page > 1).then(|| page - 1)),
        ("first", first),
        ("next", (page < pages).then_some(page + 1)),
        ("last", last),
    ]
    .into_iter()
    .filter_map(|(rel, number)| number.map(|number| (rel, number)))
    .map(|(rel, number)| format!("<{}>; rel=\"{rel}\"", page_url(request, number)))
    .collect::<Vec<String>>()
    .join(",")
}

/// The URL of page `number` of the list the request asked for, with its other query parameters
/// kept.
fn page_url(request: &ApiRequest, number: usize) -> String {
    let mut query: Vec<(String, String)> = request
        .query
        .iter()
        .filter(|(key, _)| key != "page")
        .cloned()
        .collect();
    query.push(("page".to_owned(), number.to_string()));
    query.sort_by(|one, other| one.0.cmp(&other.0));
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(query)
        .finish();
    if query.is_empty() {
        format!("{}{}", request.api_url, request.path)
    } else {
        format!("{}{}?{query}", request.api_url, request.path)
    }
}

/// Page size Forgejo uses when a request does not ask for one.
pub const PAGINATION_DEFAULT: usize = 50;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A request for the issues of `alice/tool`, with the query string `query`.
    fn request(query: &str) -> ApiRequest {
        ApiRequest {
            path: "/repos/alice/tool/issues".to_owned(),
            params: Default::default(),
            query: url::form_urlencoded::parse(query.as_bytes())
                .into_owned()
                .collect(),
            body: bytes::Bytes::new(),
            caller: None,
            api_url: "http://h/api/v1".to_owned(),
            web_url: "http://h".to_owned(),
        }
    }

    /// `count` distinguishable items.
    fn items(count: usize) -> Vec<Value> {
        (0..count).map(|item| json!(item)).collect()
    }

    /// The JSON body of `response`.
    fn json_body(response: &ApiResponse) -> &Value {
        match &response.body {
            Body::Json(value) => value,
            other => panic!("expected a JSON body, got {other:?}"),
        }
    }

    /// The header `name` of `response`.
    fn header<'a>(response: &'a ApiResponse, name: &str) -> Option<&'a str> {
        response
            .headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn a_page_carries_the_total_count_and_the_pages_around_it() {
        let first = paginate(&request("limit=2"), items(5));
        assert_eq!(first.status, 200);
        assert_eq!(json_body(&first), &json!([0, 1]));
        assert_eq!(header(&first, "x-total-count"), Some("5"));
        assert_eq!(
            header(&first, "link"),
            Some(concat!(
                "<http://h/api/v1/repos/alice/tool/issues?limit=2&page=2>; rel=\"next\",",
                "<http://h/api/v1/repos/alice/tool/issues?limit=2&page=3>; rel=\"last\"",
            ))
        );

        let middle = paginate(&request("limit=2&page=2"), items(5));
        assert_eq!(json_body(&middle), &json!([2, 3]));
        assert_eq!(
            header(&middle, "link"),
            Some(concat!(
                "<http://h/api/v1/repos/alice/tool/issues?limit=2&page=1>; rel=\"prev\",",
                "<http://h/api/v1/repos/alice/tool/issues?limit=2&page=1>; rel=\"first\",",
                "<http://h/api/v1/repos/alice/tool/issues?limit=2&page=3>; rel=\"next\",",
                "<http://h/api/v1/repos/alice/tool/issues?limit=2&page=3>; rel=\"last\"",
            ))
        );

        let last = paginate(&request("limit=2&page=3"), items(5));
        assert_eq!(json_body(&last), &json!([4]));
        let link = header(&last, "link").expect("the last page links to the one before");
        assert!(link.contains("page=2>; rel=\"prev\""), "{link}");
        assert!(link.contains("page=1>; rel=\"first\""), "{link}");
        assert!(!link.contains("rel=\"next\""), "{link}");
        let past = paginate(&request("limit=2&page=9"), items(5));
        assert_eq!(json_body(&past), &json!([]));
        assert_eq!(header(&past, "x-total-count"), Some("5"));
        let past = paginate(&request(&format!("page={}", usize::MAX)), items(5));
        assert_eq!(json_body(&past), &json!([]));
    }

    #[test]
    fn a_list_that_fits_in_one_page_has_no_links_but_a_total_count() {
        let whole = paginate(&request(""), items(5));
        assert_eq!(json_body(&whole), &json!([0, 1, 2, 3, 4]));
        assert_eq!(header(&whole, "x-total-count"), Some("5"));
        assert_eq!(header(&whole, "link"), None);
        let none = paginate(&request("state=closed"), items(0));
        assert_eq!(json_body(&none), &json!([]));
        assert_eq!(header(&none, "x-total-count"), Some("0"));
        assert_eq!(header(&none, "link"), None);
    }

    #[test]
    fn the_page_and_limit_a_request_asks_for_are_sane() {
        assert_eq!(limit_of(&request("limit=0")), PAGINATION_DEFAULT);
        assert_eq!(limit_of(&request("limit=nonsense")), PAGINATION_DEFAULT);
        assert_eq!(limit_of(&request("limit=7")), 7);
        assert_eq!(page_of(&request("page=0")), 1);
        assert_eq!(page_of(&request("page=-3")), 1);
        assert_eq!(page_of(&request("page=4")), 4);
        assert_eq!(page_of(&request("page=99999999999999999999")), 1);
        assert_eq!(page_of(&request("")), 1);
    }

    #[test]
    fn a_write_scope_grants_the_matching_read_one() {
        let caller = Caller {
            login: "alice".to_owned(),
            scopes: vec!["write:repository".to_owned()],
        };
        assert!(caller.has_scope("write:repository"));
        assert!(caller.has_scope("read:repository"));
        assert!(!caller.has_scope("write:user"));
        assert!(!caller.has_scope("read:user"));
        let reader = Caller {
            login: "bob".to_owned(),
            scopes: vec!["read:repository".to_owned()],
        };
        assert!(reader.has_scope("read:repository"));
        assert!(!reader.has_scope("write:repository"));
        assert!(!reader.has_scope("read:user"));
        assert!(!reader.has_scope("write:user"));
    }
}

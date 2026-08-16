use {
    crate::{
        structs::{HTTPFilters, HttpData, LibOptions, StatusFilter},
        utils,
    },
    futures::stream::{self, StreamExt},
    rand::{
        distr::{Alphanumeric, SampleString},
        rng,
    },
    reqwest::{
        header::{CONTENT_LENGTH, CONTENT_TYPE, USER_AGENT},
        redirect::Policy,
        Client, Response,
    },
    scraper::{Html, Selector},
    std::{
        collections::{HashMap, HashSet},
        io::{self, BufWriter, StdoutLock, Write},
        sync::OnceLock,
    },
};

/// Most of a response body that is ever read.
///
/// Applied while receiving: a body can be gigabytes, and trimming afterwards
/// would already have paid for the transfer.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Labels appended to a host when fingerprinting its answer for missing pages.
const SOFT_404_PROBE_LEN: usize = 16;

/// How many requests to keep in flight.
///
/// Never zero: `buffer_unordered(0)` has nowhere to put a future, so it polls
/// nothing and the stream never finishes.
#[must_use]
pub fn concurrency(requested: usize, work: usize) -> usize {
    requested.min(work).max(1)
}

/// How many times to try a host.
///
/// Never zero: `retries` defaults to zero, and a loop bounded by it would skip
/// the host entirely instead of checking it once.
#[must_use]
pub fn attempts(retries: usize) -> usize {
    retries.max(1)
}

/// Parsed `<title>` selector, built once instead of per response.
fn title_selector() -> &'static Selector {
    static SELECTOR: OnceLock<Selector> = OnceLock::new();
    SELECTOR.get_or_init(|| Selector::parse("title").expect("`title` is a valid selector"))
}

/// Parsed `<body>` selector, built once instead of per response.
fn body_selector() -> &'static Selector {
    static SELECTOR: OnceLock<Selector> = OnceLock::new();
    SELECTOR.get_or_init(|| Selector::parse("body").expect("`body` is a valid selector"))
}

/// Requests `url`, returning the response only when the request itself worked.
async fn attempt(options: &LibOptions, url: &str, user_agent: &str) -> Option<Response> {
    options
        .client
        .get(url)
        .header(USER_AGENT, user_agent)
        .send()
        .await
        .ok()
}

/// Fetches `host`, preferring HTTPS and falling back to plain HTTP.
///
/// Tried in order, never raced: a refused connection on port 80 wins a race
/// against a working TLS handshake and cancels it, marking the host dead.
/// Always attempts at least once, whatever `retries` says.
async fn fetch(options: &LibOptions, host: &str) -> Option<Response> {
    let user_agent = utils::return_random_user_agent(&options.user_agents);
    let secure = format!("https://{host}");
    let mut plain = None;

    for _ in 0..attempts(options.retries) {
        if let Some(response) = attempt(options, &secure, user_agent).await {
            return Some(response);
        }
        let plain_url = plain.get_or_insert_with(|| format!("http://{host}"));
        if let Some(response) = attempt(options, plain_url, user_agent).await {
            return Some(response);
        }
    }
    None
}

/// Reads at most [`MAX_BODY_BYTES`] of `response`.
async fn read_capped_body(response: Response, declared: Option<u64>) -> Vec<u8> {
    let mut response = response;
    let expected = declared
        .and_then(|len| usize::try_from(len).ok())
        .unwrap_or(0)
        .min(MAX_BODY_BYTES);
    let mut body = Vec::with_capacity(expected);

    while body.len() < MAX_BODY_BYTES {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let room = MAX_BODY_BYTES - body.len();
                let take = room.min(chunk.len());
                body.extend_from_slice(&chunk[..take]);
            }
            // A read error keeps whatever already arrived.
            Ok(None) | Err(_) => break,
        }
    }
    body
}

/// Fills in everything that can only be known by reading the body.
pub async fn assign_response_data(
    http_data: &mut HttpData,
    response: Response,
    options: &LibOptions,
) {
    let url = response.url().clone();
    let headers = response.headers();

    http_data.http_status = "ACTIVE".to_owned();
    http_data.content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let declared_length: Option<u64> = headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok()?.parse().ok());
    http_data.headers = format!("{headers:?}");

    let raw_body = read_capped_body(response, declared_length).await;

    // Content-Length is a byte count, not a character count.
    http_data.content_length = declared_length.unwrap_or(raw_body.len() as u64);

    let body = String::from_utf8_lossy(&raw_body);
    return_title_and_body(http_data, &body);

    http_data.words_count = body.split_whitespace().count();
    http_data.lines = body.lines().count() + 1;
    http_data.points_to_another_host = url.host_str() != Some(&http_data.checked_host);

    if options.return_filters {
        let host = url.host_str().unwrap_or_default().to_owned();
        http_data.bad_data = return_filters_data(&host, options).await;
    }
}

/// Extracts the title and body of an HTML document.
pub fn return_title_and_body(http_data: &mut HttpData, body: &str) {
    let document = Html::parse_document(body);

    http_data.title = document
        .select(title_selector())
        .next()
        .map_or_else(|| "NULL".to_owned(), |element| element.inner_html());

    http_data.body = document
        .select(body_selector())
        .next()
        .map_or_else(|| "NULL".to_owned(), |element| element.inner_html());
}

/// Checks every host in `options`, returning what each answered.
pub async fn return_http_data(options: &LibOptions) -> HashMap<String, HttpData> {
    if options.hosts.is_empty() {
        return HashMap::new();
    }

    let threads = concurrency(options.threads, options.hosts.len());
    let filter = StatusFilter::new(
        options.filter_codes.as_deref(),
        options.exclude_codes.as_deref(),
    );
    let mut found = HashMap::with_capacity(options.hosts.len());

    let mut checks = stream::iter(options.hosts.iter())
        .map(|host| async move {
            let mut http_data = HttpData::new(host.clone());

            if let Some(response) = fetch(options, host).await {
                http_data.protocol = response.url().scheme().to_owned();
                http_data.status_code = response.status().as_u16();
                http_data.final_url = response.url().to_string();

                if options.collect_body || options.return_filters {
                    assign_response_data(&mut http_data, response, options).await;
                } else {
                    http_data.http_status = "ACTIVE".to_owned();
                }
            } else {
                http_data.http_status = "INACTIVE".to_owned();
            }

            (host, http_data)
        })
        .buffer_unordered(threads);

    let mut out = Writer::new(options);
    while let Some((host, http_data)) = checks.next().await {
        out.report(&http_data, &filter);
        found.insert(host.clone(), http_data);
    }
    out.finish();

    found
}

/// Buffered destination for the hosts a run confirms.
///
/// One buffer for the whole run rather than a lock and a syscall per host.
struct Writer {
    sink: Option<BufWriter<StdoutLock<'static>>>,
    show_full_data: bool,
}

impl Writer {
    fn new(options: &LibOptions) -> Self {
        Self {
            sink: options
                .print_results
                .then(|| BufWriter::new(io::stdout().lock())),
            show_full_data: options.show_full_data,
        }
    }

    fn report(&mut self, http_data: &HttpData, filter: &StatusFilter) {
        let Some(sink) = self.sink.as_mut() else {
            return;
        };
        if http_data.final_url.is_empty() || !filter.accepts(http_data.status_code) {
            return;
        }

        let _ = if self.show_full_data {
            writeln!(
                sink,
                "{},[{}],[{}]",
                http_data.checked_host, http_data.final_url, http_data.status_code
            )
        } else {
            writeln!(sink, "{}://{}", http_data.protocol, http_data.checked_host)
        };
    }

    fn finish(mut self) {
        if let Some(sink) = self.sink.as_mut() {
            let _ = sink.flush();
        }
    }
}

/// Builds the HTTP client every check shares.
///
/// # Panics
///
/// Panics when the platform TLS backend cannot be initialised, which is not a
/// condition any caller can do anything about.
#[must_use]
pub fn return_http_client(timeout: u64, max_redirects: usize) -> Client {
    let policy = if max_redirects == 0 {
        Policy::none()
    } else {
        Policy::limited(max_redirects)
    };

    Client::builder()
        .timeout(std::time::Duration::from_secs(timeout))
        .redirect(policy)
        .danger_accept_invalid_certs(true)
        .use_native_tls()
        .pool_max_idle_per_host(50)
        .pool_idle_timeout(std::time::Duration::from_secs(30))
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .build()
        .expect("build the HTTP client")
}

/// Fingerprints what `host` answers for pages that cannot exist, so a later hit
/// with the same length, word count or line count can be discarded.
pub async fn return_filters_data(host: &str, options: &LibOptions) -> HTTPFilters {
    let random = Alphanumeric.sample_string(&mut rng(), SOFT_404_PROBE_LEN);

    let probes: HashSet<String> = [
        format!("{host}/admin{random}/"),
        format!("{host}/.htaccess{random}"),
        format!("{host}/{random}/"),
        format!("{host}/{random}"),
    ]
    .into_iter()
    .collect();

    // Reuses the run's client; a fresh one per host would discard the
    // connection pool and repeat the TLS setup.
    let probe_options = LibOptions {
        hosts: probes,
        client: options.client.clone(),
        // One fixed agent: the probes are compared against each other, and
        // cloning the whole list per host is not free.
        user_agents: vec![utils::return_random_user_agent(&options.user_agents).to_owned()],
        retries: 1,
        threads: 4,
        collect_body: true,
        return_filters: false,
        print_results: false,
        quiet_flag: true,
        ..LibOptions::default()
    };

    let data = Box::pin(return_http_data(&probe_options)).await;

    let mut filters = HTTPFilters::default();
    for http_data in data.values() {
        filters
            .bad_http_lengths
            .push(http_data.content_length.to_string());
        filters
            .bad_words_numbers
            .push(http_data.words_count.to_string());
        filters.bad_lines_numbers.push(http_data.lines.to_string());
    }
    filters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrency_is_never_zero() {
        assert_eq!(concurrency(0, 10), 1);
        assert_eq!(concurrency(10, 0), 1);
        assert_eq!(concurrency(0, 0), 1);
    }

    #[test]
    fn concurrency_never_exceeds_the_work_available() {
        assert_eq!(concurrency(50, 3), 3);
        assert_eq!(concurrency(3, 50), 3);
    }

    #[test]
    fn a_host_is_always_tried_at_least_once() {
        assert_eq!(attempts(0), 1);
        assert_eq!(attempts(1), 1);
        assert_eq!(attempts(5), 5);
    }

    #[test]
    fn the_selectors_parse_once_and_stay_usable() {
        assert!(std::ptr::eq(title_selector(), title_selector()));
        assert!(std::ptr::eq(body_selector(), body_selector()));
    }

    #[test]
    fn a_title_is_lifted_out_of_the_document() {
        let mut data = HttpData::new("example.com".to_owned());
        return_title_and_body(
            &mut data,
            "<html><head><title>Hi</title></head><body>x</body></html>",
        );
        assert_eq!(data.title, "Hi");
        assert_eq!(data.body, "x");
    }

    #[test]
    fn a_document_without_a_title_says_so_rather_than_guessing() {
        let mut data = HttpData::new("example.com".to_owned());
        return_title_and_body(&mut data, "<html><body>only a body</body></html>");
        assert_eq!(data.title, "NULL");
        assert_eq!(data.body, "only a body");
    }
}

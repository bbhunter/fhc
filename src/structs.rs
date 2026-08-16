use std::collections::HashSet;

#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, Debug, Eq, PartialEq, Hash, Default)]
pub struct HTTPFilters {
    pub bad_http_lengths: Vec<String>,
    pub bad_words_numbers: Vec<String>,
    pub bad_lines_numbers: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Default)]
pub struct HttpData {
    pub http_status: String,
    pub status_code: u16,
    pub checked_host: String,
    pub final_url: String,
    pub protocol: String,
    pub title: String,
    pub content_type: String,
    pub body: String,
    pub headers: String,
    pub content_length: u64,
    pub words_count: usize,
    pub lines: usize,
    pub bad_data: HTTPFilters,
    pub html_file_path: String,
    pub screenshot_data: Vec<u8>,
    pub points_to_another_host: bool,
}

impl HttpData {
    #[inline]
    #[must_use]
    pub fn new(host: String) -> Self {
        Self {
            checked_host: host,
            ..Default::default()
        }
    }
}

/// Which status codes a run wants to hear about.
///
/// Parsed once into integers: matching the raw comma separated text as a
/// substring let `40` match `404`, and cost a `to_string` per host.
#[derive(Clone, Debug, Default)]
pub struct StatusFilter {
    include: HashSet<u16>,
    exclude: HashSet<u16>,
}

impl StatusFilter {
    /// Builds the filter from the raw comma separated options.
    #[must_use]
    pub fn new(include: Option<&str>, exclude: Option<&str>) -> Self {
        Self {
            include: parse_codes(include),
            exclude: parse_codes(exclude),
        }
    }

    /// Reports whether `code` survives the filter.
    #[must_use]
    pub fn accepts(&self, code: u16) -> bool {
        if !self.include.is_empty() && !self.include.contains(&code) {
            return false;
        }
        !self.exclude.contains(&code)
    }
}

/// Reads a comma separated list of status codes, ignoring anything unparseable.
fn parse_codes(codes: Option<&str>) -> HashSet<u16> {
    codes
        .unwrap_or_default()
        .split(',')
        .filter_map(|code| code.trim().parse().ok())
        .collect()
}

#[derive(Clone, Debug, Default)]
pub struct LibOptions {
    pub hosts: HashSet<String>,
    pub client: reqwest::Client,
    pub user_agents: Vec<String>,
    pub retries: usize,
    pub threads: usize,
    /// Read the response body and everything derived from it.
    ///
    /// By far the most expensive part of a check: up to a megabyte per host,
    /// parsed as HTML. Leave off unless the body is actually read back.
    pub collect_body: bool,
    /// Fingerprint each host's answer for pages that do not exist.
    pub return_filters: bool,
    pub filter_codes: Option<String>,
    pub exclude_codes: Option<String>,
    pub show_full_data: bool,
    /// Print each result as it arrives.
    pub print_results: bool,
    pub quiet_flag: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_filter_accepts_everything() {
        let filter = StatusFilter::new(None, None);
        for code in [200, 301, 404, 500] {
            assert!(filter.accepts(code));
        }
    }

    #[test]
    fn only_the_listed_codes_get_through() {
        let filter = StatusFilter::new(Some("200,301"), None);
        assert!(filter.accepts(200));
        assert!(filter.accepts(301));
        assert!(!filter.accepts(404));
    }

    #[test]
    fn an_excluded_code_is_dropped() {
        let filter = StatusFilter::new(None, Some("404,500"));
        assert!(filter.accepts(200));
        assert!(!filter.accepts(404));
        assert!(!filter.accepts(500));
    }

    #[test]
    fn a_code_is_matched_whole_and_not_as_a_substring() {
        let filter = StatusFilter::new(Some("40"), None);
        assert!(!filter.accepts(404), "404 is not the code 40");
        assert!(filter.accepts(40), "40 itself still matches");
    }

    #[test]
    fn spacing_and_rubbish_in_the_list_are_tolerated() {
        let filter = StatusFilter::new(Some(" 200 , not-a-code ,301"), None);
        assert!(filter.accepts(200));
        assert!(filter.accepts(301));
        assert!(!filter.accepts(404));
    }

    #[test]
    fn exclusion_wins_over_inclusion() {
        let filter = StatusFilter::new(Some("200,404"), Some("404"));
        assert!(filter.accepts(200));
        assert!(!filter.accepts(404));
    }
}

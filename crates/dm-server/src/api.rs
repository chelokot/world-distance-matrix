use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use dm_core::compact::Transport;
use dm_core::geo::Coord;
use dm_wire::binary::CONTENT_TYPE as BINARY_CONTENT_TYPE;
use dm_wire::compact::CONTENT_TYPE as COMPACT_CONTENT_TYPE;
use dm_wire::request;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatrixRequest {
    coordinates: Vec<Location>,
    sources: Option<Vec<usize>>,
    destinations: Option<Vec<usize>>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
struct Location {
    lat: f64,
    lon: f64,
}

pub struct Limits {
    pub max_locations: usize,
    pub max_cells: usize,
    pub max_json_cells: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Binary,
    Compact,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Json => "json",
            Format::Binary => "binary",
            Format::Compact => "compact",
        }
    }
}

pub struct MatrixSpec {
    pub coords: Vec<Coord>,
    pub sources: Vec<usize>,
    pub destinations: Vec<usize>,
    pub format: Format,
    pub transport: Transport,
}

impl MatrixSpec {
    pub fn cells(&self) -> usize {
        self.sources.len() * self.destinations.len()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("the Accept header must allow application/json, {BINARY_CONTENT_TYPE} or {COMPACT_CONTENT_TYPE}")]
    NotAcceptable,
    #[error("{0}")]
    TooLarge(String),
    #[error("the service is at capacity, retry shortly")]
    Overloaded,
    #[error("internal error")]
    Internal,
}

impl ApiError {
    pub fn status(&self) -> StatusCode {
        match self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::NotAcceptable => StatusCode::NOT_ACCEPTABLE,
            ApiError::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            ApiError::Overloaded => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.to_string() }).to_string();
        let mut response = (self.status(), [(header::CONTENT_TYPE, "application/json")], body).into_response();
        if matches!(self, ApiError::Overloaded) {
            response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        response
    }
}

pub fn negotiate(headers: &HeaderMap) -> Result<Format, ApiError> {
    let Some(accept) = headers.get(header::ACCEPT) else { return Ok(Format::Json) };
    let accept = accept.to_str().map_err(|_| ApiError::NotAcceptable)?;
    let media_types: Vec<&str> = accept.split(',').map(|part| part.split(';').next().unwrap_or("").trim()).collect();
    if media_types.contains(&COMPACT_CONTENT_TYPE) {
        Ok(Format::Compact)
    } else if media_types.contains(&BINARY_CONTENT_TYPE) {
        Ok(Format::Binary)
    } else if media_types.iter().any(|m| matches!(*m, "application/json" | "application/*" | "*/*")) {
        Ok(Format::Json)
    } else {
        Err(ApiError::NotAcceptable)
    }
}

pub fn transport(headers: &HeaderMap) -> Transport {
    let accepts = |name: &str| {
        headers.get_all(header::ACCEPT_ENCODING).iter().filter_map(|value| value.to_str().ok()).flat_map(|value| value.split(',')).any(|coding| {
            let mut parts = coding.split(';').map(str::trim);
            parts.next() == Some(name) && parts.all(|parameter| parameter.strip_prefix("q=").and_then(|q| q.parse::<f32>().ok()).is_none_or(|q| q > 0.0))
        })
    };
    if accepts("zstd") {
        Transport::Zstd
    } else if accepts("gzip") {
        Transport::Gzip
    } else {
        Transport::Identity
    }
}

fn indices(name: &str, given: Option<Vec<usize>>, count: usize) -> Result<Vec<usize>, ApiError> {
    let indices = given.unwrap_or_else(|| (0..count).collect());
    if indices.is_empty() {
        return Err(ApiError::BadRequest(format!("{name} must not be empty")));
    }
    match indices.iter().find(|&&i| i >= count) {
        Some(bad) => Err(ApiError::BadRequest(format!("{name} index {bad} is out of range for {count} coordinates"))),
        None => Ok(indices),
    }
}

impl MatrixRequest {
    pub fn parse(headers: &HeaderMap, body: &[u8]) -> Result<Self, ApiError> {
        let invalid = |reason: String| ApiError::BadRequest(format!("invalid request body: {reason}"));
        let content_type = headers.get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).and_then(|value| value.split(';').next());
        if content_type.map(str::trim) != Some(request::CONTENT_TYPE) {
            return serde_json::from_slice(body).map_err(|e| invalid(e.to_string()));
        }
        let decoded = request::decode(body).map_err(invalid)?;
        let indices = |list: Option<Vec<u32>>| list.map(|list| list.into_iter().map(|index| index as usize).collect());
        Ok(Self {
            coordinates: decoded.coordinates.iter().map(|&(lat, lon)| Location { lat: lat as f64 / 1e7, lon: lon as f64 / 1e7 }).collect(),
            sources: indices(decoded.sources),
            destinations: indices(decoded.destinations),
        })
    }

    pub fn validate(self, format: Format, limits: &Limits) -> Result<MatrixSpec, ApiError> {
        let count = self.coordinates.len();
        if count == 0 {
            return Err(ApiError::BadRequest("coordinates must not be empty".into()));
        }
        if count > limits.max_locations {
            return Err(ApiError::TooLarge(format!("{count} coordinates exceed the limit of {}", limits.max_locations)));
        }
        let coords = self
            .coordinates
            .iter()
            .enumerate()
            .map(|(index, location)| {
                let valid = (-90.0..=90.0).contains(&location.lat) && (-180.0..=180.0).contains(&location.lon);
                if valid {
                    Ok(Coord::from_degrees(location.lat, location.lon))
                } else {
                    Err(ApiError::BadRequest(format!("coordinate {index} is outside the valid lat/lon range")))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let spec = MatrixSpec {
            coords,
            sources: indices("sources", self.sources, count)?,
            destinations: indices("destinations", self.destinations, count)?,
            format,
            transport: Transport::Identity,
        };
        let limit = match format {
            Format::Json => limits.max_json_cells,
            Format::Binary | Format::Compact => limits.max_cells,
        };
        if spec.cells() > limit {
            let hint = if format == Format::Json { format!("; request {BINARY_CONTENT_TYPE} for larger matrices") } else { String::new() };
            return Err(ApiError::TooLarge(format!("{} matrix cells exceed the {} limit of {limit}{hint}", spec.cells(), format.label())));
        }
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits { max_locations: 10, max_cells: 50, max_json_cells: 20 };

    fn request(json: &str) -> MatrixRequest {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn square_by_default() {
        let spec = request(r#"{"coordinates":[{"lat":54,"lon":10},{"lat":54.1,"lon":10.1}]}"#).validate(Format::Json, &LIMITS).unwrap();
        assert_eq!(spec.sources, vec![0, 1]);
        assert_eq!(spec.destinations, vec![0, 1]);
    }

    #[test]
    fn rejects_invalid_input() {
        let bad = |json: &str, format| request(json).validate(format, &LIMITS).err().unwrap();
        assert!(matches!(bad(r#"{"coordinates":[]}"#, Format::Json), ApiError::BadRequest(_)));
        assert!(matches!(bad(r#"{"coordinates":[{"lat":91,"lon":0}]}"#, Format::Json), ApiError::BadRequest(_)));
        assert!(matches!(bad(r#"{"coordinates":[{"lat":1,"lon":0}],"sources":[1]}"#, Format::Json), ApiError::BadRequest(_)));
        assert!(matches!(bad(r#"{"coordinates":[{"lat":1,"lon":0}],"sources":[]}"#, Format::Json), ApiError::BadRequest(_)));
        let five = r#"{"coordinates":[{"lat":1,"lon":0},{"lat":1,"lon":0},{"lat":1,"lon":0},{"lat":1,"lon":0},{"lat":1,"lon":0}]}"#;
        assert!(matches!(bad(five, Format::Json), ApiError::TooLarge(_)));
        assert!(request(five).validate(Format::Binary, &LIMITS).is_ok());
        assert!(serde_json::from_str::<MatrixRequest>(r#"{"coordinates":[{"lat":1,"lng":0}]}"#).is_err());
    }

    #[test]
    fn rectangular_requests_are_counted_by_cells() {
        let json = r#"{"coordinates":[{"lat":1,"lon":0},{"lat":1,"lon":0},{"lat":1,"lon":0},{"lat":1,"lon":0},{"lat":1,"lon":0}],"sources":[4]}"#;
        assert_eq!(request(json).validate(Format::Json, &LIMITS).unwrap().cells(), 5);
    }

    #[test]
    fn prefers_zstd_then_gzip() {
        let chosen = |value: &'static str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static(value));
            transport(&headers)
        };
        assert_eq!(chosen("gzip, deflate, br, zstd"), Transport::Zstd);
        assert_eq!(chosen("zstd;q=0.5"), Transport::Zstd);
        assert_eq!(chosen("gzip, zstd;q=0"), Transport::Gzip);
        assert_eq!(chosen("br, deflate"), Transport::Identity);
        assert_eq!(transport(&HeaderMap::new()), Transport::Identity);
    }

    #[test]
    fn negotiates_formats() {
        let mut headers = HeaderMap::new();
        assert_eq!(negotiate(&headers).unwrap(), Format::Json);
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/vnd.distance-matrix.v1, application/json;q=0.5"));
        assert_eq!(negotiate(&headers).unwrap(), Format::Binary);
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/vnd.distance-matrix.compact.v1"));
        assert_eq!(negotiate(&headers).unwrap(), Format::Compact);
        headers.insert(header::ACCEPT, HeaderValue::from_static("*/*"));
        assert_eq!(negotiate(&headers).unwrap(), Format::Json);
        headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
        assert!(negotiate(&headers).is_err());
    }
}

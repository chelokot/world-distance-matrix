use crate::words;

pub const CONTENT_TYPE: &str = "application/vnd.distance-matrix.request.v1";
pub const MAGIC: [u8; 4] = *b"DMQ1";
const HEADER_LEN: usize = 16;

#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub coordinates: Vec<(i32, i32)>,
    pub sources: Option<Vec<u32>>,
    pub destinations: Option<Vec<u32>>,
}

pub fn encode(request: &Request) -> Vec<u8> {
    let count = |list: &Option<Vec<u32>>| list.as_ref().map_or(0, Vec::len) as u32;
    let header = [request.coordinates.len() as u32, count(&request.sources), count(&request.destinations)];
    let coordinates = request.coordinates.iter().flat_map(|&(lat, lon)| [lat as u32, lon as u32]);
    let indices = request.sources.iter().chain(&request.destinations).flatten().copied();
    MAGIC.into_iter().chain(header.into_iter().chain(coordinates).chain(indices).flat_map(u32::to_le_bytes)).collect()
}

pub fn decode(body: &[u8]) -> Result<Request, String> {
    if body.len() < HEADER_LEN || body[..4] != MAGIC {
        return Err("not a binary matrix request".into());
    }
    let [count, sources, destinations] = words(&body[4..HEADER_LEN])[..] else { unreachable!("three words") };
    let (count, sources, destinations) = (count as usize, sources as usize, destinations as usize);
    if body.len() != HEADER_LEN + 4 * (2 * count + sources + destinations) {
        return Err(format!("body does not hold {count} coordinates, {sources} sources and {destinations} destinations"));
    }
    let values = words(&body[HEADER_LEN..]);
    let (coordinates, indices) = values.split_at(2 * count);
    let list = |values: &[u32]| (!values.is_empty()).then(|| values.to_vec());
    Ok(Request {
        coordinates: coordinates.as_chunks::<2>().0.iter().map(|&[lat, lon]| (lat as i32, lon as i32)).collect(),
        sources: list(&indices[..sources]),
        destinations: list(&indices[sources..]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_and_without_index_lists() {
        for (sources, destinations) in [(None, None), (Some(vec![2, 0]), None), (None, Some(vec![1])), (Some(vec![0]), Some(vec![2, 1]))] {
            let request = Request { coordinates: vec![(540_000_000, 100_000_000), (-339_249_000, 184_241_000), (1, -1_800_000_000)], sources, destinations };
            assert_eq!(decode(&encode(&request)).unwrap(), request);
        }
        assert_eq!(encode(&Request { coordinates: vec![(0, 0); 1000], sources: None, destinations: None }).len(), 8_016);
    }

    #[test]
    fn rejects_bodies_of_the_wrong_length() {
        let body = encode(&Request { coordinates: vec![(1, 2)], sources: Some(vec![0]), destinations: None });
        assert!(decode(&body[..body.len() - 1]).is_err());
        assert!(decode(b"DMQ1").is_err());
    }
}

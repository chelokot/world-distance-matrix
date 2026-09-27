pub const PROFILE_NAME: &str = "car-v2";
pub const TRAFFIC_SIGNAL_PENALTY_MS: u32 = 2_000;
const MAXSPEED_FACTOR: f64 = 0.8;
const UNPAVED_SPEED_CAP_KMH: f64 = 30.0;
const FERRY_DEFAULT_SPEED_KMH: f64 = 20.0;
const MPH: f64 = 1.609344;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pace {
    Speed { forward_kmh: f64, backward_kmh: f64 },
    FixedDuration { seconds: f64 },
    DefaultFerry,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WayProfile {
    pub forward: bool,
    pub backward: bool,
    pub pace: Pace,
}

impl WayProfile {
    pub fn speeds_kmh(&self, way_length_m: f64) -> (f64, f64) {
        match self.pace {
            Pace::Speed { forward_kmh, backward_kmh } => (forward_kmh, backward_kmh),
            Pace::FixedDuration { seconds } => {
                let kmh = (way_length_m / 1000.0) / (seconds / 3600.0);
                (kmh, kmh)
            }
            Pace::DefaultFerry => (FERRY_DEFAULT_SPEED_KMH, FERRY_DEFAULT_SPEED_KMH),
        }
    }
}

fn default_speed_kmh(highway: &str) -> Option<f64> {
    Some(match highway {
        "motorway" => 90.0,
        "motorway_link" => 45.0,
        "trunk" => 85.0,
        "trunk_link" => 40.0,
        "primary" => 65.0,
        "primary_link" => 30.0,
        "secondary" => 55.0,
        "secondary_link" => 25.0,
        "tertiary" => 40.0,
        "tertiary_link" => 20.0,
        "unclassified" | "residential" => 25.0,
        "living_street" => 10.0,
        "service" => 15.0,
        _ => return None,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Access {
    Allowed,
    Destination,
    Denied,
    Unspecified,
}

fn access_value(value: &str) -> Access {
    let denied = |part: &str| {
        matches!(part.trim(), "no" | "private" | "agricultural" | "forestry" | "emergency" | "psv" | "bus" | "military" | "permit" | "use_sidepath")
    };
    let destination = |part: &str| denied(part) || matches!(part.trim(), "destination" | "delivery" | "customers");
    if value.split(';').all(denied) {
        Access::Denied
    } else if value.split(';').all(destination) {
        Access::Destination
    } else {
        Access::Allowed
    }
}

#[derive(Default)]
pub struct Tags<'a> {
    highway: Option<&'a str>,
    route: Option<&'a str>,
    junction: Option<&'a str>,
    oneway: Option<&'a str>,
    access: Option<&'a str>,
    vehicle: Option<&'a str>,
    motor_vehicle: Option<&'a str>,
    motorcar: Option<&'a str>,
    hgv: Option<&'a str>,
    maxspeed: Option<&'a str>,
    maxspeed_forward: Option<&'a str>,
    maxspeed_backward: Option<&'a str>,
    service: Option<&'a str>,
    duration: Option<&'a str>,
    area: Option<&'a str>,
    impassable: Option<&'a str>,
    surface: Option<&'a str>,
    barrier: Option<&'a str>,
    relation_type: Option<&'a str>,
    restriction: Option<&'a str>,
    restriction_motor_vehicle: Option<&'a str>,
    restriction_motorcar: Option<&'a str>,
    except: Option<&'a str>,
}

impl<'a> Tags<'a> {
    pub fn collect(tags: impl Iterator<Item = (&'a str, &'a str)>) -> Self {
        let mut collected = Tags::default();
        for (key, value) in tags {
            let slot = match key {
                "highway" => &mut collected.highway,
                "route" => &mut collected.route,
                "junction" => &mut collected.junction,
                "oneway" => &mut collected.oneway,
                "access" => &mut collected.access,
                "vehicle" => &mut collected.vehicle,
                "motor_vehicle" => &mut collected.motor_vehicle,
                "motorcar" => &mut collected.motorcar,
                "hgv" => &mut collected.hgv,
                "maxspeed" => &mut collected.maxspeed,
                "maxspeed:forward" => &mut collected.maxspeed_forward,
                "maxspeed:backward" => &mut collected.maxspeed_backward,
                "service" => &mut collected.service,
                "duration" => &mut collected.duration,
                "area" => &mut collected.area,
                "impassable" => &mut collected.impassable,
                "surface" => &mut collected.surface,
                "barrier" => &mut collected.barrier,
                "type" => &mut collected.relation_type,
                "restriction" => &mut collected.restriction,
                "restriction:motor_vehicle" => &mut collected.restriction_motor_vehicle,
                "restriction:motorcar" => &mut collected.restriction_motorcar,
                "except" => &mut collected.except,
                _ => continue,
            };
            *slot = Some(value);
        }
        collected
    }

    fn car_access(&self) -> Access {
        [self.motorcar, self.motor_vehicle, self.vehicle, self.access].into_iter().flatten().next().map_or(Access::Unspecified, access_value)
    }
}

fn parse_maxspeed(value: &str) -> Option<f64> {
    let value = value.trim();
    if let Some((country, kind)) = value.split_once(':') {
        return implicit_maxspeed(country, kind);
    }
    match value {
        "none" => return Some(140.0),
        "walk" => return Some(7.0),
        _ => {}
    }
    let (number, unit_mph) = match value.strip_suffix("mph") {
        Some(number) => (number, true),
        None => (value.trim_end_matches("km/h").trim_end_matches("kmh").trim_end_matches("kph"), false),
    };
    let speed: f64 = number.trim().parse().ok()?;
    let speed = if unit_mph { speed * MPH } else { speed };
    (speed > 0.0 && speed < 300.0).then_some(speed)
}

fn implicit_maxspeed(country: &str, kind: &str) -> Option<f64> {
    let uk = matches!(country, "GB" | "UK");
    Some(match kind {
        "urban" if uk => 30.0 * MPH,
        "urban" => 50.0,
        "nsl_single" => 60.0 * MPH,
        "nsl_dual" | "motorway" if uk => 70.0 * MPH,
        "rural" => match country {
            "DE" | "AT" => 100.0,
            "NL" | "FR" | "ES" | "PT" => 80.0,
            _ => 90.0,
        },
        "trunk" => 100.0,
        "motorway" => match country {
            "DE" | "NL" | "FR" | "AT" | "IT" | "PL" | "CZ" | "DK" => 130.0,
            _ => 120.0,
        },
        "living_street" => 7.0,
        "bicycle_road" | "cyclestreet" => 30.0,
        "zone30" | "zone:30" => 30.0,
        "zone20" | "zone:20" => 20.0,
        _ => {
            let digits = kind.trim_start_matches("zone:").trim_start_matches("zone");
            return digits.parse().ok();
        }
    })
}

fn parse_duration_seconds(value: &str) -> Option<f64> {
    let value = value.trim();
    if let Some(iso) = value.strip_prefix("PT") {
        let mut seconds = 0.0;
        let mut number = String::new();
        for c in iso.chars() {
            match c {
                '0'..='9' | '.' => number.push(c),
                'H' | 'M' | 'S' => {
                    let n: f64 = number.parse().ok()?;
                    seconds += n * match c {
                        'H' => 3600.0,
                        'M' => 60.0,
                        _ => 1.0,
                    };
                    number.clear();
                }
                _ => return None,
            }
        }
        return (seconds > 0.0).then_some(seconds);
    }
    let parts: Vec<f64> = value.split(':').map(|p| p.trim().parse::<f64>()).collect::<Result<_, _>>().ok()?;
    let seconds = match parts.as_slice() {
        [minutes] => minutes * 60.0,
        [hours, minutes] => hours * 3600.0 + minutes * 60.0,
        [hours, minutes, seconds] => hours * 3600.0 + minutes * 60.0 + seconds,
        _ => return None,
    };
    (seconds > 0.0).then_some(seconds)
}

fn directions(tags: &Tags) -> Option<(bool, bool)> {
    let implied_oneway = matches!(tags.junction, Some("roundabout" | "circular")) || tags.highway == Some("motorway");
    match tags.oneway {
        Some("yes" | "1" | "true") => Some((true, false)),
        Some("-1" | "reverse") => Some((false, true)),
        Some("reversible") => None,
        Some("no" | "0" | "false" | "alternating") => Some((true, true)),
        _ if implied_oneway => Some((true, false)),
        _ => Some((true, true)),
    }
}

pub fn way_profile(tags: &Tags) -> Option<WayProfile> {
    if matches!(tags.route, Some("ferry" | "shuttle_train")) {
        let explicitly_allowed = [tags.motorcar, tags.motor_vehicle, tags.vehicle, tags.hgv, tags.access]
            .into_iter()
            .flatten()
            .next()
            .map(access_value)
            .is_some_and(|access| matches!(access, Access::Allowed | Access::Destination));
        if tags.route == Some("ferry") && !explicitly_allowed || tags.car_access() == Access::Denied {
            return None;
        }
        let pace = match (tags.duration.and_then(parse_duration_seconds), tags.maxspeed.and_then(parse_maxspeed)) {
            (Some(seconds), _) => Pace::FixedDuration { seconds },
            (None, Some(kmh)) => Pace::Speed { forward_kmh: kmh, backward_kmh: kmh },
            (None, None) => Pace::DefaultFerry,
        };
        let (forward, backward) = directions(tags)?;
        return Some(WayProfile { forward, backward, pace });
    }
    let highway = tags.highway?;
    let class_speed = default_speed_kmh(highway)?;
    if tags.area == Some("yes") || tags.impassable == Some("yes") || tags.car_access() == Access::Denied {
        return None;
    }
    if highway == "service"
        && (matches!(tags.service, Some("parking_aisle" | "driveway" | "drive-through" | "emergency_access")) || tags.car_access() == Access::Destination)
    {
        return None;
    }
    let (forward, backward) = directions(tags)?;
    let unpaved = matches!(
        tags.surface,
        Some("unpaved" | "gravel" | "fine_gravel" | "compacted" | "dirt" | "earth" | "ground" | "grass" | "mud" | "sand" | "pebblestone" | "rock")
    );
    let speed_for = |directional: Option<&str>| {
        let posted = directional.or(tags.maxspeed).and_then(parse_maxspeed);
        let speed = posted.map_or(class_speed, |limit| limit * MAXSPEED_FACTOR);
        if unpaved {
            speed.min(UNPAVED_SPEED_CAP_KMH)
        } else {
            speed
        }
    };
    Some(WayProfile { forward, backward, pace: Pace::Speed { forward_kmh: speed_for(tags.maxspeed_forward), backward_kmh: speed_for(tags.maxspeed_backward) } })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnRule {
    Forbid,
    Only,
}

pub fn turn_rule(tags: &Tags) -> Option<TurnRule> {
    if tags.relation_type != Some("restriction") {
        return None;
    }
    if tags.except.is_some_and(|except| except.split(';').any(|v| matches!(v.trim(), "motorcar" | "motor_vehicle"))) {
        return None;
    }
    let value = tags.restriction_motorcar.or(tags.restriction_motor_vehicle).or(tags.restriction)?;
    if value.starts_with("no_") {
        Some(TurnRule::Forbid)
    } else if value.starts_with("only_") {
        Some(TurnRule::Only)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeTraits {
    pub blocks_cars: bool,
    pub traffic_signal: bool,
}

pub fn node_traits(tags: &Tags) -> NodeTraits {
    let blocks_cars = tags.barrier.is_some_and(|barrier| {
        let passable_kind = matches!(
            barrier,
            "cattle_grid"
                | "border_control"
                | "toll_booth"
                | "sally_port"
                | "gate"
                | "lift_gate"
                | "no"
                | "entrance"
                | "height_restrictor"
                | "arch"
                | "swing_gate"
                | "sliding_gate"
        );
        match tags.car_access() {
            Access::Denied => true,
            Access::Allowed | Access::Destination => false,
            Access::Unspecified => !passable_kind,
        }
    });
    NodeTraits { blocks_cars, traffic_signal: tags.highway == Some("traffic_signals") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(tags: &[(&str, &str)]) -> Option<WayProfile> {
        way_profile(&Tags::collect(tags.iter().copied()))
    }

    fn forward_speed(p: WayProfile) -> f64 {
        p.speeds_kmh(1000.0).0
    }

    #[test]
    fn classifies_common_roads() {
        let residential = profile(&[("highway", "residential")]).unwrap();
        assert!(residential.forward && residential.backward);
        assert_eq!(forward_speed(residential), 25.0);
        assert!(profile(&[("highway", "footway")]).is_none());
        assert!(profile(&[("highway", "track")]).is_none());
        assert!(profile(&[("highway", "residential"), ("access", "private")]).is_none());
        assert!(profile(&[("highway", "residential"), ("access", "no"), ("motor_vehicle", "destination")]).is_some());
        assert!(profile(&[("highway", "service"), ("service", "parking_aisle")]).is_none());
        assert!(profile(&[("highway", "service"), ("service", "alley")]).is_some());
        assert!(profile(&[("highway", "residential"), ("access", "agricultural;forestry")]).is_none());
        assert!(profile(&[("highway", "residential"), ("access", "destination;delivery")]).is_some());
        assert!(profile(&[("highway", "service"), ("access", "customers")]).is_none());
        assert!(profile(&[("highway", "service"), ("motor_vehicle", "agricultural;destination")]).is_none());
        assert!(profile(&[("highway", "service"), ("access", "no"), ("motor_vehicle", "yes")]).is_some());
    }

    #[test]
    fn honours_oneway_rules() {
        let oneway = profile(&[("highway", "primary"), ("oneway", "yes")]).unwrap();
        assert!(oneway.forward && !oneway.backward);
        let reverse = profile(&[("highway", "primary"), ("oneway", "-1")]).unwrap();
        assert!(!reverse.forward && reverse.backward);
        let roundabout = profile(&[("highway", "primary"), ("junction", "roundabout")]).unwrap();
        assert!(roundabout.forward && !roundabout.backward);
        let motorway = profile(&[("highway", "motorway")]).unwrap();
        assert!(motorway.forward && !motorway.backward);
        let two_way_motorway = profile(&[("highway", "motorway"), ("oneway", "no")]).unwrap();
        assert!(two_way_motorway.backward);
        assert!(profile(&[("highway", "primary"), ("oneway", "reversible")]).is_none());
    }

    #[test]
    fn parses_speed_limits() {
        assert_eq!(parse_maxspeed("50"), Some(50.0));
        assert!((parse_maxspeed("30 mph").unwrap() - 48.28).abs() < 0.01);
        assert_eq!(parse_maxspeed("DE:urban"), Some(50.0));
        assert_eq!(parse_maxspeed("DE:rural"), Some(100.0));
        assert!((parse_maxspeed("GB:nsl_single").unwrap() - 96.56).abs() < 0.01);
        assert_eq!(parse_maxspeed("DE:zone:30"), Some(30.0));
        assert_eq!(parse_maxspeed("none"), Some(140.0));
        assert_eq!(parse_maxspeed("signals"), None);
        let limited = profile(&[("highway", "primary"), ("maxspeed", "50")]).unwrap();
        assert_eq!(forward_speed(limited), 40.0);
        let directional = profile(&[("highway", "primary"), ("maxspeed", "100"), ("maxspeed:backward", "50")]).unwrap();
        assert_eq!(directional.speeds_kmh(1.0), (80.0, 40.0));
        let gravel = profile(&[("highway", "unclassified"), ("maxspeed", "100"), ("surface", "gravel")]).unwrap();
        assert_eq!(forward_speed(gravel), 30.0);
    }

    #[test]
    fn ferries_need_explicit_car_access() {
        assert!(profile(&[("route", "ferry")]).is_none());
        assert!(profile(&[("route", "ferry"), ("motor_vehicle", "no")]).is_none());
        let ferry = profile(&[("route", "ferry"), ("motorcar", "yes"), ("duration", "01:30")]).unwrap();
        assert_eq!(ferry.pace, Pace::FixedDuration { seconds: 5400.0 });
        assert!((forward_speed(ferry) - 1.0 / 1.5).abs() < 1e-9);
        let default_ferry = profile(&[("route", "ferry"), ("motor_vehicle", "yes")]).unwrap();
        assert_eq!(default_ferry.pace, Pace::DefaultFerry);
        assert!(profile(&[("route", "shuttle_train")]).is_some());
        assert!(profile(&[("route", "ferry"), ("hgv", "yes")]).is_some());
        let shuttle = profile(&[("route", "shuttle_train"), ("motorcar", "yes"), ("maxspeed", "100"), ("oneway", "yes")]).unwrap();
        assert_eq!((shuttle.pace, shuttle.backward), (Pace::Speed { forward_kmh: 100.0, backward_kmh: 100.0 }, false));
        assert!(profile(&[("route", "ferry"), ("foot", "yes"), ("vehicle", "no"), ("hgv", "yes")]).is_none());
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration_seconds("45"), Some(2700.0));
        assert_eq!(parse_duration_seconds("1:05:30"), Some(3930.0));
        assert_eq!(parse_duration_seconds("PT1H30M"), Some(5400.0));
        assert_eq!(parse_duration_seconds("soon"), None);
    }

    #[test]
    fn reads_turn_restrictions_for_cars() {
        let rule = |tags: &[(&str, &str)]| turn_rule(&Tags::collect(tags.iter().copied()));
        assert_eq!(rule(&[("type", "restriction"), ("restriction", "no_left_turn")]), Some(TurnRule::Forbid));
        assert_eq!(rule(&[("type", "restriction"), ("restriction", "only_straight_on")]), Some(TurnRule::Only));
        assert_eq!(rule(&[("type", "restriction"), ("restriction:motorcar", "no_u_turn")]), Some(TurnRule::Forbid));
        assert_eq!(rule(&[("type", "restriction"), ("restriction", "no_right_turn"), ("except", "bicycle;motorcar")]), None);
        assert_eq!(rule(&[("type", "restriction"), ("restriction:bicycle", "no_left_turn")]), None);
        assert_eq!(rule(&[("type", "multipolygon"), ("restriction", "no_left_turn")]), None);
    }

    #[test]
    fn barriers_block_unless_passable() {
        let traits = |tags: &[(&str, &str)]| node_traits(&Tags::collect(tags.iter().copied()));
        assert!(traits(&[("barrier", "bollard")]).blocks_cars);
        assert!(!traits(&[("barrier", "bollard"), ("motor_vehicle", "yes")]).blocks_cars);
        assert!(!traits(&[("barrier", "gate")]).blocks_cars);
        assert!(!traits(&[("barrier", "bollard"), ("motor_vehicle", "destination")]).blocks_cars);
        assert!(traits(&[("barrier", "gate"), ("access", "private")]).blocks_cars);
        assert!(!traits(&[("highway", "crossing")]).blocks_cars);
        assert!(traits(&[("highway", "traffic_signals")]).traffic_signal);
    }
}

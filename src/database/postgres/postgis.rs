use std::borrow::Cow;

pub(super) fn is_spatial_type(type_name: &str) -> bool {
    matches!(type_name, "geometry" | "geography")
}

/// PostGIS emits hex EWKB through its PostgreSQL text output function unless the
/// caller has explicitly requested WKT. Keep malformed or already-text values intact.
pub(super) fn display_value(value: &str) -> Cow<'_, str> {
    ewkb_hex_to_wkt(value)
        .map(Cow::Owned)
        .unwrap_or(Cow::Borrowed(value))
}

fn ewkb_hex_to_wkt(value: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let (pairs, remainder) = value.trim().as_bytes().as_chunks::<2>();
    for pair in pairs {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        bytes.push((high << 4) | low);
    }
    if !remainder.is_empty() {
        return None;
    }

    let mut reader = Reader::new(&bytes);
    let geometry = reader.geometry().ok()?;
    if !reader.remaining().is_empty() {
        return None;
    }
    Some(geometry.wkt())
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.offset..]
    }

    fn geometry(&mut self) -> Result<Geometry, ()> {
        let byte_order = self.byte()?;
        let little_endian = match byte_order {
            0 => false,
            1 => true,
            _ => return Err(()),
        };
        let type_word = self.u32(little_endian)?;
        let dimensions = dimensions(type_word).ok_or(())?;
        let base_type = geometry_type(type_word).ok_or(())?;
        let has_srid = type_word & 0x2000_0000 != 0;
        let has_bbox = type_word & 0x1000_0000 != 0;
        if has_srid {
            self.u32(little_endian)?;
        }
        if has_bbox {
            for _ in 0..dimensions.coordinate_count() * 2 {
                self.f64(little_endian)?;
            }
        }

        let payload = match base_type {
            GeometryType::Point => {
                GeometryPayload::Point(self.coordinate(little_endian, dimensions)?)
            }
            GeometryType::LineString | GeometryType::CircularString => {
                GeometryPayload::Coordinates(self.coordinates(little_endian, dimensions)?)
            }
            GeometryType::Polygon | GeometryType::Triangle => {
                GeometryPayload::Rings(self.rings(little_endian, dimensions)?)
            }
            GeometryType::MultiPoint
            | GeometryType::MultiLineString
            | GeometryType::MultiPolygon
            | GeometryType::GeometryCollection
            | GeometryType::CompoundCurve
            | GeometryType::CurvePolygon
            | GeometryType::MultiCurve
            | GeometryType::MultiSurface
            | GeometryType::PolyhedralSurface
            | GeometryType::Tin => {
                let count = self.count(little_endian)?;
                let mut geometries = Vec::with_capacity(count.min(1024));
                for _ in 0..count {
                    geometries.push(self.geometry()?);
                }
                GeometryPayload::Geometries(geometries)
            }
        };
        Ok(Geometry {
            geometry_type: base_type,
            dimensions,
            payload,
        })
    }

    fn count(&mut self, little_endian: bool) -> Result<usize, ()> {
        let length = self.take(4)?;
        let value = if little_endian {
            u32::from_le_bytes(length.try_into().map_err(|_| ())?)
        } else {
            u32::from_be_bytes(length.try_into().map_err(|_| ())?)
        } as usize;
        if value > self.remaining().len().saturating_add(1) {
            return Err(());
        }
        Ok(value)
    }

    fn rings(
        &mut self,
        little_endian: bool,
        dimensions: Dimensions,
    ) -> Result<Vec<Vec<Coordinate>>, ()> {
        let count = self.count(little_endian)?;
        let mut rings = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            rings.push(self.coordinates(little_endian, dimensions)?);
        }
        Ok(rings)
    }

    fn coordinates(
        &mut self,
        little_endian: bool,
        dimensions: Dimensions,
    ) -> Result<Vec<Coordinate>, ()> {
        let count = self.count(little_endian)?;
        let mut coordinates = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            coordinates.push(self.coordinate(little_endian, dimensions)?);
        }
        Ok(coordinates)
    }

    fn coordinate(
        &mut self,
        little_endian: bool,
        dimensions: Dimensions,
    ) -> Result<Coordinate, ()> {
        let x = self.f64(little_endian)?;
        let y = self.f64(little_endian)?;
        let z = dimensions
            .has_z()
            .then(|| self.f64(little_endian))
            .transpose()?;
        let m = dimensions
            .has_m()
            .then(|| self.f64(little_endian))
            .transpose()?;
        Ok(Coordinate { x, y, z, m })
    }

    fn byte(&mut self) -> Result<u8, ()> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self, little_endian: bool) -> Result<u32, ()> {
        let bytes = self.take(4)?;
        if little_endian {
            Ok(u32::from_le_bytes(bytes.try_into().map_err(|_| ())?))
        } else {
            Ok(u32::from_be_bytes(bytes.try_into().map_err(|_| ())?))
        }
    }

    fn f64(&mut self, little_endian: bool) -> Result<f64, ()> {
        let bytes = self.take(8)?;
        let value = if little_endian {
            u64::from_le_bytes(bytes.try_into().map_err(|_| ())?)
        } else {
            u64::from_be_bytes(bytes.try_into().map_err(|_| ())?)
        };
        Ok(f64::from_bits(value))
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], ()> {
        let end = self.offset.checked_add(len).ok_or(())?;
        let bytes = self.bytes.get(self.offset..end).ok_or(())?;
        self.offset = end;
        Ok(bytes)
    }
}

#[derive(Clone, Copy)]
struct Dimensions {
    z: bool,
    m: bool,
}

impl Dimensions {
    fn coordinate_count(self) -> usize {
        2 + usize::from(self.z) + usize::from(self.m)
    }

    fn has_z(self) -> bool {
        self.z
    }

    fn has_m(self) -> bool {
        self.m
    }

    fn suffix(self) -> &'static str {
        match (self.z, self.m) {
            (false, false) => "",
            (true, false) => " Z",
            (false, true) => " M",
            (true, true) => " ZM",
        }
    }
}

fn dimensions(type_word: u32) -> Option<Dimensions> {
    if type_word & 0xc000_0000 != 0 {
        return Some(Dimensions {
            z: type_word & 0x8000_0000 != 0,
            m: type_word & 0x4000_0000 != 0,
        });
    }
    match (type_word & 0x0fff_ffff) / 1000 {
        0 => Some(Dimensions { z: false, m: false }),
        1 => Some(Dimensions { z: true, m: false }),
        2 => Some(Dimensions { z: false, m: true }),
        3 => Some(Dimensions { z: true, m: true }),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum GeometryType {
    Point,
    LineString,
    Polygon,
    MultiPoint,
    MultiLineString,
    MultiPolygon,
    GeometryCollection,
    CircularString,
    CompoundCurve,
    CurvePolygon,
    MultiCurve,
    MultiSurface,
    PolyhedralSurface,
    Tin,
    Triangle,
}

impl GeometryType {
    fn name(self) -> &'static str {
        match self {
            Self::Point => "POINT",
            Self::LineString => "LINESTRING",
            Self::Polygon => "POLYGON",
            Self::MultiPoint => "MULTIPOINT",
            Self::MultiLineString => "MULTILINESTRING",
            Self::MultiPolygon => "MULTIPOLYGON",
            Self::GeometryCollection => "GEOMETRYCOLLECTION",
            Self::CircularString => "CIRCULARSTRING",
            Self::CompoundCurve => "COMPOUNDCURVE",
            Self::CurvePolygon => "CURVEPOLYGON",
            Self::MultiCurve => "MULTICURVE",
            Self::MultiSurface => "MULTISURFACE",
            Self::PolyhedralSurface => "POLYHEDRALSURFACE",
            Self::Tin => "TIN",
            Self::Triangle => "TRIANGLE",
        }
    }
}

fn geometry_type(type_word: u32) -> Option<GeometryType> {
    let base = if type_word & 0xe000_0000 != 0 {
        type_word & 0x0fff_ffff
    } else {
        (type_word & 0x0fff_ffff) % 1000
    };
    match base {
        1 => Some(GeometryType::Point),
        2 => Some(GeometryType::LineString),
        3 => Some(GeometryType::Polygon),
        4 => Some(GeometryType::MultiPoint),
        5 => Some(GeometryType::MultiLineString),
        6 => Some(GeometryType::MultiPolygon),
        7 => Some(GeometryType::GeometryCollection),
        8 => Some(GeometryType::CircularString),
        9 => Some(GeometryType::CompoundCurve),
        10 => Some(GeometryType::CurvePolygon),
        11 => Some(GeometryType::MultiCurve),
        12 => Some(GeometryType::MultiSurface),
        15 => Some(GeometryType::PolyhedralSurface),
        16 => Some(GeometryType::Tin),
        17 => Some(GeometryType::Triangle),
        _ => None,
    }
}

struct Geometry {
    geometry_type: GeometryType,
    dimensions: Dimensions,
    payload: GeometryPayload,
}

impl Geometry {
    fn wkt(&self) -> String {
        let mut output = String::new();
        self.write_full(&mut output);
        output
    }

    fn write_full(&self, output: &mut String) {
        output.push_str(self.geometry_type.name());
        output.push_str(self.dimensions.suffix());
        output.push(' ');
        self.write_body(output);
    }

    fn write_body(&self, output: &mut String) {
        match &self.payload {
            GeometryPayload::Point(coordinate) => {
                if coordinate.is_empty() {
                    output.push_str("EMPTY");
                } else {
                    output.push('(');
                    coordinate.write(output);
                    output.push(')');
                }
            }
            GeometryPayload::Coordinates(coordinates) => write_point_sequence(output, coordinates),
            GeometryPayload::Rings(rings) => write_rings(output, rings),
            GeometryPayload::Geometries(geometries) => match self.geometry_type {
                GeometryType::MultiPoint => {
                    write_collection(output, geometries, |output, geometry| {
                        geometry.write_body(output)
                    });
                }
                GeometryType::MultiLineString => {
                    write_collection(output, geometries, |output, geometry| {
                        geometry.write_body(output)
                    });
                }
                GeometryType::MultiPolygon
                | GeometryType::PolyhedralSurface
                | GeometryType::Tin => {
                    write_collection(output, geometries, |output, geometry| {
                        geometry.write_body(output)
                    });
                }
                _ => write_collection(output, geometries, |output, geometry| {
                    geometry.write_full(output)
                }),
            },
        }
    }
}

fn write_collection(
    output: &mut String,
    geometries: &[Geometry],
    mut write: impl FnMut(&mut String, &Geometry),
) {
    if geometries.is_empty() {
        output.push_str("EMPTY");
        return;
    }
    output.push('(');
    for (index, geometry) in geometries.iter().enumerate() {
        if index > 0 {
            output.push_str(", ");
        }
        write(output, geometry);
    }
    output.push(')');
}

fn write_point_sequence(output: &mut String, coordinates: &[Coordinate]) {
    if coordinates.is_empty() {
        output.push_str("EMPTY");
        return;
    }
    output.push('(');
    for (index, coordinate) in coordinates.iter().enumerate() {
        if index > 0 {
            output.push_str(", ");
        }
        coordinate.write(output);
    }
    output.push(')');
}

fn write_rings(output: &mut String, rings: &[Vec<Coordinate>]) {
    if rings.is_empty() {
        output.push_str("EMPTY");
        return;
    }
    output.push('(');
    for (index, ring) in rings.iter().enumerate() {
        if index > 0 {
            output.push_str(", ");
        }
        write_point_sequence(output, ring);
    }
    output.push(')');
}

enum GeometryPayload {
    Point(Coordinate),
    Coordinates(Vec<Coordinate>),
    Rings(Vec<Vec<Coordinate>>),
    Geometries(Vec<Geometry>),
}

struct Coordinate {
    x: f64,
    y: f64,
    z: Option<f64>,
    m: Option<f64>,
}

impl Coordinate {
    fn is_empty(&self) -> bool {
        self.x.is_nan()
            && self.y.is_nan()
            && self.z.is_none_or(f64::is_nan)
            && self.m.is_none_or(f64::is_nan)
    }

    fn write(&self, output: &mut String) {
        write_number(output, self.x);
        output.push(' ');
        write_number(output, self.y);
        if let Some(z) = self.z {
            output.push(' ');
            write_number(output, z);
        }
        if let Some(m) = self.m {
            output.push(' ');
            write_number(output, m);
        }
    }
}

fn write_number(output: &mut String, value: f64) {
    if value.is_infinite() {
        if value.is_sign_negative() {
            output.push('-');
        }
        output.push_str("Infinity");
    } else if value.is_nan() {
        output.push_str("NaN");
    } else {
        output.push_str(&value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::{display_value, is_spatial_type};

    fn wkt(value: &str) -> String {
        display_value(value).into_owned()
    }

    #[test]
    fn recognizes_postgis_types_without_treating_other_types_as_spatial() {
        assert!(is_spatial_type("geometry"));
        assert!(is_spatial_type("geography"));
        assert!(!is_spatial_type("geometry_dump"));
        assert!(!is_spatial_type("text"));
    }

    #[test]
    fn converts_common_ewkb_geometries_to_wkt() {
        assert_eq!(
            wkt("0101000000000000000000F03F0000000000000040"),
            "POINT (1 2)"
        );
        assert_eq!(
            wkt(
                "010200000002000000000000000000F03F000000000000004000000000000008400000000000001040"
            ),
            "LINESTRING (1 2, 3 4)"
        );
        assert_eq!(
            wkt(
                "0103000000010000000400000000000000000000000000000000000000000000000000F03F00000000000000000000000000000840000000000000F03F00000000000000000000000000000000"
            ),
            "POLYGON ((0 0, 1 0, 3 1, 0 0))"
        );
    }

    #[test]
    fn converts_dimensions_srid_and_nested_geometries() {
        assert_eq!(
            wkt("0101000020E6100000000000000000F03F0000000000000040"),
            "POINT (1 2)"
        );
        assert_eq!(
            wkt("0101000080000000000000F03F00000000000000400000000000000840"),
            "POINT Z (1 2 3)"
        );
        assert_eq!(
            wkt("0104000000010000000101000000000000000000F03F0000000000000040"),
            "MULTIPOINT ((1 2))"
        );
        assert_eq!(wkt("010700000000000000"), "GEOMETRYCOLLECTION EMPTY");
    }

    #[test]
    fn keeps_wkt_and_non_hex_values_unchanged() {
        assert_eq!(wkt("POINT(1 2)"), "POINT(1 2)");
        assert_eq!(wkt("not geometry bytes"), "not geometry bytes");
        assert!(matches!(display_value("abc"), Cow::Borrowed("abc")));
    }

    #[test]
    fn converts_postgis_empty_points() {
        assert_eq!(
            wkt("0101000000000000000000f87f000000000000f87f"),
            "POINT EMPTY"
        );
    }

    use std::borrow::Cow;
}

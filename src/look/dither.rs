//! Ordered threshold maps and error-diffusion kernels from Image-to-Pixel
//! (<https://github.com/Tezumie/Image-to-Pixel>, MIT).

/// How the host mixes palette colours after it has averaged each block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dither {
    None,
    Bayer2,
    #[default]
    Bayer4,
    Bayer8,
    Bayer16,
    Cluster4,
    Cluster8,
    Lines,
    BlueNoise,
    FloydSteinberg,
    Atkinson,
    SierraLite,
    Sierra,
    Burkes,
    Jarvis,
    Stucki,
}

impl Dither {
    pub const ALL: [Dither; 16] = [
        Self::None,
        Self::Bayer2,
        Self::Bayer4,
        Self::Bayer8,
        Self::Bayer16,
        Self::Cluster4,
        Self::Cluster8,
        Self::Lines,
        Self::BlueNoise,
        Self::FloydSteinberg,
        Self::Atkinson,
        Self::SierraLite,
        Self::Sierra,
        Self::Burkes,
        Self::Jarvis,
        Self::Stucki,
    ];

    pub fn token(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Bayer2 => "bayer2",
            Self::Bayer4 => "bayer4",
            Self::Bayer8 => "bayer8",
            Self::Bayer16 => "bayer16",
            Self::Cluster4 => "cluster4",
            Self::Cluster8 => "cluster8",
            Self::Lines => "lines",
            Self::BlueNoise => "bluenoise",
            Self::FloydSteinberg => "floyd-steinberg",
            Self::Atkinson => "atkinson",
            Self::SierraLite => "sierra-lite",
            Self::Sierra => "sierra",
            Self::Burkes => "burkes",
            Self::Jarvis => "jarvis",
            Self::Stucki => "stucki",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|row| row.token() == token)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Bayer2 => "Bayer 2×2",
            Self::Bayer4 => "Bayer 4×4",
            Self::Bayer8 => "Bayer 8×8",
            Self::Bayer16 => "Bayer 16×16",
            Self::Cluster4 => "Cluster 4×4",
            Self::Cluster8 => "Cluster 8×8",
            Self::Lines => "Lines",
            Self::BlueNoise => "Blue noise",
            Self::FloydSteinberg => "Floyd–Steinberg",
            Self::Atkinson => "Atkinson",
            Self::SierraLite => "Sierra Lite",
            Self::Sierra => "Sierra",
            Self::Burkes => "Burkes",
            Self::Jarvis => "Jarvis",
            Self::Stucki => "Stucki",
        }
    }

    pub fn is_diffusion(self) -> bool {
        matches!(
            self,
            Self::FloydSteinberg
                | Self::Atkinson
                | Self::SierraLite
                | Self::Sierra
                | Self::Burkes
                | Self::Jarvis
                | Self::Stucki
        )
    }
}

pub(crate) struct Map {
    pub w: usize,
    pub h: usize,
    pub levels: u32,
    pub values: &'static [u8],
}

pub(crate) fn threshold_map(dither: Dither) -> Option<Map> {
    match dither {
        Dither::Bayer2 => Some(Map {
            w: 2,
            h: 2,
            levels: 4,
            values: &BAYER2,
        }),
        Dither::Bayer4 => Some(Map {
            w: 4,
            h: 4,
            levels: 16,
            values: &BAYER4,
        }),
        Dither::Bayer8 => Some(Map {
            w: 8,
            h: 8,
            levels: 64,
            values: &BAYER8,
        }),
        Dither::Bayer16 => Some(Map {
            w: 16,
            h: 16,
            levels: 256,
            values: &BAYER16,
        }),
        Dither::Cluster4 => Some(Map {
            w: 4,
            h: 4,
            levels: 16,
            values: &CLUSTER4,
        }),
        Dither::Cluster8 => Some(Map {
            w: 8,
            h: 8,
            levels: 64,
            values: &CLUSTER8,
        }),
        Dither::Lines => Some(Map {
            w: 1,
            h: 4,
            levels: 4,
            values: &LINES,
        }),
        Dither::BlueNoise => Some(Map {
            w: 64,
            h: 64,
            levels: 256,
            values: BLUE_NOISE,
        }),
        Dither::None
        | Dither::FloydSteinberg
        | Dither::Atkinson
        | Dither::SierraLite
        | Dither::Sierra
        | Dither::Burkes
        | Dither::Jarvis
        | Dither::Stucki => None,
    }
}

/// `[dx, dy, weight]` as in Image-to-Pixel `KERNELS`.
pub(crate) fn kernel(dither: Dither) -> &'static [(i32, i32, f32)] {
    match dither {
        Dither::FloydSteinberg => &[
            (1, 0, 7.0 / 16.0),
            (-1, 1, 3.0 / 16.0),
            (0, 1, 5.0 / 16.0),
            (1, 1, 1.0 / 16.0),
        ],
        Dither::Atkinson => &[
            (1, 0, 1.0 / 8.0),
            (2, 0, 1.0 / 8.0),
            (-1, 1, 1.0 / 8.0),
            (0, 1, 1.0 / 8.0),
            (1, 1, 1.0 / 8.0),
            (0, 2, 1.0 / 8.0),
        ],
        Dither::SierraLite => &[(1, 0, 2.0 / 4.0), (-1, 1, 1.0 / 4.0), (0, 1, 1.0 / 4.0)],
        Dither::Sierra => &[
            (1, 0, 5.0 / 32.0),
            (2, 0, 3.0 / 32.0),
            (-2, 1, 2.0 / 32.0),
            (-1, 1, 4.0 / 32.0),
            (0, 1, 5.0 / 32.0),
            (1, 1, 4.0 / 32.0),
            (2, 1, 2.0 / 32.0),
            (-1, 2, 2.0 / 32.0),
            (0, 2, 3.0 / 32.0),
            (1, 2, 2.0 / 32.0),
        ],
        Dither::Burkes => &[
            (1, 0, 8.0 / 32.0),
            (2, 0, 4.0 / 32.0),
            (-2, 1, 2.0 / 32.0),
            (-1, 1, 4.0 / 32.0),
            (0, 1, 8.0 / 32.0),
            (1, 1, 4.0 / 32.0),
            (2, 1, 2.0 / 32.0),
        ],
        Dither::Jarvis => &[
            (1, 0, 7.0 / 48.0),
            (2, 0, 5.0 / 48.0),
            (-2, 1, 3.0 / 48.0),
            (-1, 1, 5.0 / 48.0),
            (0, 1, 7.0 / 48.0),
            (1, 1, 5.0 / 48.0),
            (2, 1, 3.0 / 48.0),
            (-2, 2, 1.0 / 48.0),
            (-1, 2, 3.0 / 48.0),
            (0, 2, 5.0 / 48.0),
            (1, 2, 3.0 / 48.0),
            (2, 2, 1.0 / 48.0),
        ],
        Dither::Stucki => &[
            (1, 0, 8.0 / 42.0),
            (2, 0, 4.0 / 42.0),
            (-2, 1, 2.0 / 42.0),
            (-1, 1, 4.0 / 42.0),
            (0, 1, 8.0 / 42.0),
            (1, 1, 4.0 / 42.0),
            (2, 1, 2.0 / 42.0),
            (-2, 2, 1.0 / 42.0),
            (-1, 2, 2.0 / 42.0),
            (0, 2, 4.0 / 42.0),
            (1, 2, 2.0 / 42.0),
            (2, 2, 1.0 / 42.0),
        ],
        _ => &[],
    }
}

const BAYER2: [u8; 4] = [0, 2, 3, 1];
pub(crate) const BAYER4: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];
const BAYER8: [u8; 64] = [
    0, 32, 8, 40, 2, 34, 10, 42, 48, 16, 56, 24, 50, 18, 58, 26, 12, 44, 4, 36, 14, 46, 6, 38, 60,
    28, 52, 20, 62, 30, 54, 22, 3, 35, 11, 43, 1, 33, 9, 41, 51, 19, 59, 27, 49, 17, 57, 25, 15,
    47, 7, 39, 13, 45, 5, 37, 63, 31, 55, 23, 61, 29, 53, 21,
];
const BAYER16: [u8; 256] = [
    0, 128, 32, 160, 8, 136, 40, 168, 2, 130, 34, 162, 10, 138, 42, 170, 192, 64, 224, 96, 200, 72,
    232, 104, 194, 66, 226, 98, 202, 74, 234, 106, 48, 176, 16, 144, 56, 184, 24, 152, 50, 178, 18,
    146, 58, 186, 26, 154, 240, 112, 208, 80, 248, 120, 216, 88, 242, 114, 210, 82, 250, 122, 218,
    90, 12, 140, 44, 172, 4, 132, 36, 164, 14, 142, 46, 174, 6, 134, 38, 166, 204, 76, 236, 108,
    196, 68, 228, 100, 206, 78, 238, 110, 198, 70, 230, 102, 60, 188, 28, 156, 52, 180, 20, 148,
    62, 190, 30, 158, 54, 182, 22, 150, 252, 124, 220, 92, 244, 116, 212, 84, 254, 126, 222, 94,
    246, 118, 214, 86, 3, 131, 35, 163, 11, 139, 43, 171, 1, 129, 33, 161, 9, 137, 41, 169, 195,
    67, 227, 99, 203, 75, 235, 107, 193, 65, 225, 97, 201, 73, 233, 105, 51, 179, 19, 147, 59, 187,
    27, 155, 49, 177, 17, 145, 57, 185, 25, 153, 243, 115, 211, 83, 251, 123, 219, 91, 241, 113,
    209, 81, 249, 121, 217, 89, 15, 143, 47, 175, 7, 135, 39, 167, 13, 141, 45, 173, 5, 133, 37,
    165, 207, 79, 239, 111, 199, 71, 231, 103, 205, 77, 237, 109, 197, 69, 229, 101, 63, 191, 31,
    159, 55, 183, 23, 151, 61, 189, 29, 157, 53, 181, 21, 149, 255, 127, 223, 95, 247, 119, 215,
    87, 253, 125, 221, 93, 245, 117, 213, 85,
];
/// Ranks of Image-to-Pixel's classic 4×4 clustered-dot matrix.
const CLUSTER4: [u8; 16] = [6, 12, 10, 3, 11, 15, 13, 7, 9, 14, 5, 1, 4, 8, 2, 0];
/// Ranks of the cosine clustered-dot screen of size 8.
const CLUSTER8: [u8; 64] = [
    0, 6, 18, 30, 32, 20, 8, 2, 4, 12, 28, 42, 44, 34, 14, 10, 16, 26, 48, 54, 56, 50, 36, 22, 24,
    40, 52, 60, 62, 58, 46, 38, 25, 41, 53, 61, 63, 59, 47, 39, 17, 27, 49, 55, 57, 51, 37, 23, 5,
    13, 29, 43, 45, 35, 15, 11, 1, 7, 19, 31, 33, 21, 9, 3,
];
const LINES: [u8; 4] = [0, 2, 1, 3];
const BLUE_NOISE: &[u8] = include_bytes!("bluenoise.bin");

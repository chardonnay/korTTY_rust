// Graphic range of IBM CCSID 37. Control bytes are interpreted by the
// screen protocol, independently of the EBCDIC character conversion.
const GRAPHICS: [char; 191] = [
    '\u{20}', '\u{a0}', '\u{e2}', '\u{e4}', '\u{e0}', '\u{e1}', '\u{e3}', '\u{e5}', '\u{e7}',
    '\u{f1}', '\u{a2}', '\u{2e}', '\u{3c}', '\u{28}', '\u{2b}', '\u{7c}', '\u{26}', '\u{e9}',
    '\u{ea}', '\u{eb}', '\u{e8}', '\u{ed}', '\u{ee}', '\u{ef}', '\u{ec}', '\u{df}', '\u{21}',
    '\u{24}', '\u{2a}', '\u{29}', '\u{3b}', '\u{ac}', '\u{2d}', '\u{2f}', '\u{c2}', '\u{c4}',
    '\u{c0}', '\u{c1}', '\u{c3}', '\u{c5}', '\u{c7}', '\u{d1}', '\u{a6}', '\u{2c}', '\u{25}',
    '\u{5f}', '\u{3e}', '\u{3f}', '\u{f8}', '\u{c9}', '\u{ca}', '\u{cb}', '\u{c8}', '\u{cd}',
    '\u{ce}', '\u{cf}', '\u{cc}', '\u{60}', '\u{3a}', '\u{23}', '\u{40}', '\u{27}', '\u{3d}',
    '\u{22}', '\u{d8}', '\u{61}', '\u{62}', '\u{63}', '\u{64}', '\u{65}', '\u{66}', '\u{67}',
    '\u{68}', '\u{69}', '\u{ab}', '\u{bb}', '\u{f0}', '\u{fd}', '\u{fe}', '\u{b1}', '\u{b0}',
    '\u{6a}', '\u{6b}', '\u{6c}', '\u{6d}', '\u{6e}', '\u{6f}', '\u{70}', '\u{71}', '\u{72}',
    '\u{aa}', '\u{ba}', '\u{e6}', '\u{b8}', '\u{c6}', '\u{a4}', '\u{b5}', '\u{7e}', '\u{73}',
    '\u{74}', '\u{75}', '\u{76}', '\u{77}', '\u{78}', '\u{79}', '\u{7a}', '\u{a1}', '\u{bf}',
    '\u{d0}', '\u{dd}', '\u{de}', '\u{ae}', '\u{5e}', '\u{a3}', '\u{a5}', '\u{b7}', '\u{a9}',
    '\u{a7}', '\u{b6}', '\u{bc}', '\u{bd}', '\u{be}', '\u{5b}', '\u{5d}', '\u{af}', '\u{a8}',
    '\u{b4}', '\u{d7}', '\u{7b}', '\u{41}', '\u{42}', '\u{43}', '\u{44}', '\u{45}', '\u{46}',
    '\u{47}', '\u{48}', '\u{49}', '\u{ad}', '\u{f4}', '\u{f6}', '\u{f2}', '\u{f3}', '\u{f5}',
    '\u{7d}', '\u{4a}', '\u{4b}', '\u{4c}', '\u{4d}', '\u{4e}', '\u{4f}', '\u{50}', '\u{51}',
    '\u{52}', '\u{b9}', '\u{fb}', '\u{fc}', '\u{f9}', '\u{fa}', '\u{ff}', '\u{5c}', '\u{f7}',
    '\u{53}', '\u{54}', '\u{55}', '\u{56}', '\u{57}', '\u{58}', '\u{59}', '\u{5a}', '\u{b2}',
    '\u{d4}', '\u{d6}', '\u{d2}', '\u{d3}', '\u{d5}', '\u{30}', '\u{31}', '\u{32}', '\u{33}',
    '\u{34}', '\u{35}', '\u{36}', '\u{37}', '\u{38}', '\u{39}', '\u{b3}', '\u{db}', '\u{dc}',
    '\u{d9}', '\u{da}',
];

pub(crate) fn decode(byte: u8) -> char {
    match byte {
        0x40..=0xfe => GRAPHICS[usize::from(byte - 0x40)],
        0x1c => '∗',
        0x1e => ';',
        0x3f => '●',
        _ => ' ',
    }
}

pub(crate) fn encode(character: char) -> Option<u8> {
    GRAPHICS
        .iter()
        .position(|&value| value == character)
        .and_then(|index| u8::try_from(index + 0x40).ok())
}

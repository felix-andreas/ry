//! The graphics engine and device API.
//!
//! ry has no graphics engine yet (an open question in the runtime design), but packages that
//! implement or use devices (ragg, systemfonts, textshaping) import the engine's names and must
//! load, register their routines, and run the parts that do not draw. So every engine entry
//! point is exported: the color and text helpers work, and anything that needs a device raises
//! an R error that says graphics devices are not supported yet.

use crate::{
    boundary::{ApiResult, enter, error, raise},
    runtime,
    strings::{c_bytes, utf8_text},
    vectors::{int_elt, real_elt, xlength},
};
use heap::object::{NA_INTEGER, Sexp, SexpType, is_na_real};
use std::ffi::{c_char, c_int, c_uint};

/// The graphics engine version ry presents: R 4.6's (`R_GE_fontVar`).
pub const GE_VERSION: c_int = 17;

/// R's packed colour: red in the low byte, then green, blue and alpha.
pub type RColor = c_uint;

const TRANSPARENT_WHITE: RColor = 0x00FF_FFFF;
const OPAQUE: RColor = 0xFF00_0000;

/// The default palette of R 4.0 and later.
const PALETTE: [RColor; 8] = [
    0xFF00_0000,
    0xFF6B_53DF,
    0xFF4F_D061,
    0xFFE6_9722,
    0xFFE5_E228,
    0xFFBC_0BCD,
    0xFF10_C7F5,
    0xFF9E_9E9E,
];

/// R's colour names with their colours as `0xBBGGRR`, in the order `colors()` lists them.
const COLOR_NAMES: &[(&str, u32)] = &[
    ("white", 0xFFFFFF),
    ("aliceblue", 0xFFF8F0),
    ("antiquewhite", 0xD7EBFA),
    ("antiquewhite1", 0xDBEFFF),
    ("antiquewhite2", 0xCCDFEE),
    ("antiquewhite3", 0xB0C0CD),
    ("antiquewhite4", 0x78838B),
    ("aquamarine", 0xD4FF7F),
    ("aquamarine1", 0xD4FF7F),
    ("aquamarine2", 0xC6EE76),
    ("aquamarine3", 0xAACD66),
    ("aquamarine4", 0x748B45),
    ("azure", 0xFFFFF0),
    ("azure1", 0xFFFFF0),
    ("azure2", 0xEEEEE0),
    ("azure3", 0xCDCDC1),
    ("azure4", 0x8B8B83),
    ("beige", 0xDCF5F5),
    ("bisque", 0xC4E4FF),
    ("bisque1", 0xC4E4FF),
    ("bisque2", 0xB7D5EE),
    ("bisque3", 0x9EB7CD),
    ("bisque4", 0x6B7D8B),
    ("black", 0x000000),
    ("blanchedalmond", 0xCDEBFF),
    ("blue", 0xFF0000),
    ("blue1", 0xFF0000),
    ("blue2", 0xEE0000),
    ("blue3", 0xCD0000),
    ("blue4", 0x8B0000),
    ("blueviolet", 0xE22B8A),
    ("brown", 0x2A2AA5),
    ("brown1", 0x4040FF),
    ("brown2", 0x3B3BEE),
    ("brown3", 0x3333CD),
    ("brown4", 0x23238B),
    ("burlywood", 0x87B8DE),
    ("burlywood1", 0x9BD3FF),
    ("burlywood2", 0x91C5EE),
    ("burlywood3", 0x7DAACD),
    ("burlywood4", 0x55738B),
    ("cadetblue", 0xA09E5F),
    ("cadetblue1", 0xFFF598),
    ("cadetblue2", 0xEEE58E),
    ("cadetblue3", 0xCDC57A),
    ("cadetblue4", 0x8B8653),
    ("chartreuse", 0x00FF7F),
    ("chartreuse1", 0x00FF7F),
    ("chartreuse2", 0x00EE76),
    ("chartreuse3", 0x00CD66),
    ("chartreuse4", 0x008B45),
    ("chocolate", 0x1E69D2),
    ("chocolate1", 0x247FFF),
    ("chocolate2", 0x2176EE),
    ("chocolate3", 0x1D66CD),
    ("chocolate4", 0x13458B),
    ("coral", 0x507FFF),
    ("coral1", 0x5672FF),
    ("coral2", 0x506AEE),
    ("coral3", 0x455BCD),
    ("coral4", 0x2F3E8B),
    ("cornflowerblue", 0xED9564),
    ("cornsilk", 0xDCF8FF),
    ("cornsilk1", 0xDCF8FF),
    ("cornsilk2", 0xCDE8EE),
    ("cornsilk3", 0xB1C8CD),
    ("cornsilk4", 0x78888B),
    ("cyan", 0xFFFF00),
    ("cyan1", 0xFFFF00),
    ("cyan2", 0xEEEE00),
    ("cyan3", 0xCDCD00),
    ("cyan4", 0x8B8B00),
    ("darkblue", 0x8B0000),
    ("darkcyan", 0x8B8B00),
    ("darkgoldenrod", 0x0B86B8),
    ("darkgoldenrod1", 0x0FB9FF),
    ("darkgoldenrod2", 0x0EADEE),
    ("darkgoldenrod3", 0x0C95CD),
    ("darkgoldenrod4", 0x08658B),
    ("darkgray", 0xA9A9A9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xA9A9A9),
    ("darkkhaki", 0x6BB7BD),
    ("darkmagenta", 0x8B008B),
    ("darkolivegreen", 0x2F6B55),
    ("darkolivegreen1", 0x70FFCA),
    ("darkolivegreen2", 0x68EEBC),
    ("darkolivegreen3", 0x5ACDA2),
    ("darkolivegreen4", 0x3D8B6E),
    ("darkorange", 0x008CFF),
    ("darkorange1", 0x007FFF),
    ("darkorange2", 0x0076EE),
    ("darkorange3", 0x0066CD),
    ("darkorange4", 0x00458B),
    ("darkorchid", 0xCC3299),
    ("darkorchid1", 0xFF3EBF),
    ("darkorchid2", 0xEE3AB2),
    ("darkorchid3", 0xCD329A),
    ("darkorchid4", 0x8B2268),
    ("darkred", 0x00008B),
    ("darksalmon", 0x7A96E9),
    ("darkseagreen", 0x8FBC8F),
    ("darkseagreen1", 0xC1FFC1),
    ("darkseagreen2", 0xB4EEB4),
    ("darkseagreen3", 0x9BCD9B),
    ("darkseagreen4", 0x698B69),
    ("darkslateblue", 0x8B3D48),
    ("darkslategray", 0x4F4F2F),
    ("darkslategray1", 0xFFFF97),
    ("darkslategray2", 0xEEEE8D),
    ("darkslategray3", 0xCDCD79),
    ("darkslategray4", 0x8B8B52),
    ("darkslategrey", 0x4F4F2F),
    ("darkturquoise", 0xD1CE00),
    ("darkviolet", 0xD30094),
    ("deeppink", 0x9314FF),
    ("deeppink1", 0x9314FF),
    ("deeppink2", 0x8912EE),
    ("deeppink3", 0x7610CD),
    ("deeppink4", 0x500A8B),
    ("deepskyblue", 0xFFBF00),
    ("deepskyblue1", 0xFFBF00),
    ("deepskyblue2", 0xEEB200),
    ("deepskyblue3", 0xCD9A00),
    ("deepskyblue4", 0x8B6800),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0xFF901E),
    ("dodgerblue1", 0xFF901E),
    ("dodgerblue2", 0xEE861C),
    ("dodgerblue3", 0xCD7418),
    ("dodgerblue4", 0x8B4E10),
    ("firebrick", 0x2222B2),
    ("firebrick1", 0x3030FF),
    ("firebrick2", 0x2C2CEE),
    ("firebrick3", 0x2626CD),
    ("firebrick4", 0x1A1A8B),
    ("floralwhite", 0xF0FAFF),
    ("forestgreen", 0x228B22),
    ("gainsboro", 0xDCDCDC),
    ("ghostwhite", 0xFFF8F8),
    ("gold", 0x00D7FF),
    ("gold1", 0x00D7FF),
    ("gold2", 0x00C9EE),
    ("gold3", 0x00ADCD),
    ("gold4", 0x00758B),
    ("goldenrod", 0x20A5DA),
    ("goldenrod1", 0x25C1FF),
    ("goldenrod2", 0x22B4EE),
    ("goldenrod3", 0x1D9BCD),
    ("goldenrod4", 0x14698B),
    ("gray", 0xBEBEBE),
    ("gray0", 0x000000),
    ("gray1", 0x030303),
    ("gray2", 0x050505),
    ("gray3", 0x080808),
    ("gray4", 0x0A0A0A),
    ("gray5", 0x0D0D0D),
    ("gray6", 0x0F0F0F),
    ("gray7", 0x121212),
    ("gray8", 0x141414),
    ("gray9", 0x171717),
    ("gray10", 0x1A1A1A),
    ("gray11", 0x1C1C1C),
    ("gray12", 0x1F1F1F),
    ("gray13", 0x212121),
    ("gray14", 0x242424),
    ("gray15", 0x262626),
    ("gray16", 0x292929),
    ("gray17", 0x2B2B2B),
    ("gray18", 0x2E2E2E),
    ("gray19", 0x303030),
    ("gray20", 0x333333),
    ("gray21", 0x363636),
    ("gray22", 0x383838),
    ("gray23", 0x3B3B3B),
    ("gray24", 0x3D3D3D),
    ("gray25", 0x404040),
    ("gray26", 0x424242),
    ("gray27", 0x454545),
    ("gray28", 0x474747),
    ("gray29", 0x4A4A4A),
    ("gray30", 0x4D4D4D),
    ("gray31", 0x4F4F4F),
    ("gray32", 0x525252),
    ("gray33", 0x545454),
    ("gray34", 0x575757),
    ("gray35", 0x595959),
    ("gray36", 0x5C5C5C),
    ("gray37", 0x5E5E5E),
    ("gray38", 0x616161),
    ("gray39", 0x636363),
    ("gray40", 0x666666),
    ("gray41", 0x696969),
    ("gray42", 0x6B6B6B),
    ("gray43", 0x6E6E6E),
    ("gray44", 0x707070),
    ("gray45", 0x737373),
    ("gray46", 0x757575),
    ("gray47", 0x787878),
    ("gray48", 0x7A7A7A),
    ("gray49", 0x7D7D7D),
    ("gray50", 0x7F7F7F),
    ("gray51", 0x828282),
    ("gray52", 0x858585),
    ("gray53", 0x878787),
    ("gray54", 0x8A8A8A),
    ("gray55", 0x8C8C8C),
    ("gray56", 0x8F8F8F),
    ("gray57", 0x919191),
    ("gray58", 0x949494),
    ("gray59", 0x969696),
    ("gray60", 0x999999),
    ("gray61", 0x9C9C9C),
    ("gray62", 0x9E9E9E),
    ("gray63", 0xA1A1A1),
    ("gray64", 0xA3A3A3),
    ("gray65", 0xA6A6A6),
    ("gray66", 0xA8A8A8),
    ("gray67", 0xABABAB),
    ("gray68", 0xADADAD),
    ("gray69", 0xB0B0B0),
    ("gray70", 0xB3B3B3),
    ("gray71", 0xB5B5B5),
    ("gray72", 0xB8B8B8),
    ("gray73", 0xBABABA),
    ("gray74", 0xBDBDBD),
    ("gray75", 0xBFBFBF),
    ("gray76", 0xC2C2C2),
    ("gray77", 0xC4C4C4),
    ("gray78", 0xC7C7C7),
    ("gray79", 0xC9C9C9),
    ("gray80", 0xCCCCCC),
    ("gray81", 0xCFCFCF),
    ("gray82", 0xD1D1D1),
    ("gray83", 0xD4D4D4),
    ("gray84", 0xD6D6D6),
    ("gray85", 0xD9D9D9),
    ("gray86", 0xDBDBDB),
    ("gray87", 0xDEDEDE),
    ("gray88", 0xE0E0E0),
    ("gray89", 0xE3E3E3),
    ("gray90", 0xE5E5E5),
    ("gray91", 0xE8E8E8),
    ("gray92", 0xEBEBEB),
    ("gray93", 0xEDEDED),
    ("gray94", 0xF0F0F0),
    ("gray95", 0xF2F2F2),
    ("gray96", 0xF5F5F5),
    ("gray97", 0xF7F7F7),
    ("gray98", 0xFAFAFA),
    ("gray99", 0xFCFCFC),
    ("gray100", 0xFFFFFF),
    ("green", 0x00FF00),
    ("green1", 0x00FF00),
    ("green2", 0x00EE00),
    ("green3", 0x00CD00),
    ("green4", 0x008B00),
    ("greenyellow", 0x2FFFAD),
    ("grey", 0xBEBEBE),
    ("grey0", 0x000000),
    ("grey1", 0x030303),
    ("grey2", 0x050505),
    ("grey3", 0x080808),
    ("grey4", 0x0A0A0A),
    ("grey5", 0x0D0D0D),
    ("grey6", 0x0F0F0F),
    ("grey7", 0x121212),
    ("grey8", 0x141414),
    ("grey9", 0x171717),
    ("grey10", 0x1A1A1A),
    ("grey11", 0x1C1C1C),
    ("grey12", 0x1F1F1F),
    ("grey13", 0x212121),
    ("grey14", 0x242424),
    ("grey15", 0x262626),
    ("grey16", 0x292929),
    ("grey17", 0x2B2B2B),
    ("grey18", 0x2E2E2E),
    ("grey19", 0x303030),
    ("grey20", 0x333333),
    ("grey21", 0x363636),
    ("grey22", 0x383838),
    ("grey23", 0x3B3B3B),
    ("grey24", 0x3D3D3D),
    ("grey25", 0x404040),
    ("grey26", 0x424242),
    ("grey27", 0x454545),
    ("grey28", 0x474747),
    ("grey29", 0x4A4A4A),
    ("grey30", 0x4D4D4D),
    ("grey31", 0x4F4F4F),
    ("grey32", 0x525252),
    ("grey33", 0x545454),
    ("grey34", 0x575757),
    ("grey35", 0x595959),
    ("grey36", 0x5C5C5C),
    ("grey37", 0x5E5E5E),
    ("grey38", 0x616161),
    ("grey39", 0x636363),
    ("grey40", 0x666666),
    ("grey41", 0x696969),
    ("grey42", 0x6B6B6B),
    ("grey43", 0x6E6E6E),
    ("grey44", 0x707070),
    ("grey45", 0x737373),
    ("grey46", 0x757575),
    ("grey47", 0x787878),
    ("grey48", 0x7A7A7A),
    ("grey49", 0x7D7D7D),
    ("grey50", 0x7F7F7F),
    ("grey51", 0x828282),
    ("grey52", 0x858585),
    ("grey53", 0x878787),
    ("grey54", 0x8A8A8A),
    ("grey55", 0x8C8C8C),
    ("grey56", 0x8F8F8F),
    ("grey57", 0x919191),
    ("grey58", 0x949494),
    ("grey59", 0x969696),
    ("grey60", 0x999999),
    ("grey61", 0x9C9C9C),
    ("grey62", 0x9E9E9E),
    ("grey63", 0xA1A1A1),
    ("grey64", 0xA3A3A3),
    ("grey65", 0xA6A6A6),
    ("grey66", 0xA8A8A8),
    ("grey67", 0xABABAB),
    ("grey68", 0xADADAD),
    ("grey69", 0xB0B0B0),
    ("grey70", 0xB3B3B3),
    ("grey71", 0xB5B5B5),
    ("grey72", 0xB8B8B8),
    ("grey73", 0xBABABA),
    ("grey74", 0xBDBDBD),
    ("grey75", 0xBFBFBF),
    ("grey76", 0xC2C2C2),
    ("grey77", 0xC4C4C4),
    ("grey78", 0xC7C7C7),
    ("grey79", 0xC9C9C9),
    ("grey80", 0xCCCCCC),
    ("grey81", 0xCFCFCF),
    ("grey82", 0xD1D1D1),
    ("grey83", 0xD4D4D4),
    ("grey84", 0xD6D6D6),
    ("grey85", 0xD9D9D9),
    ("grey86", 0xDBDBDB),
    ("grey87", 0xDEDEDE),
    ("grey88", 0xE0E0E0),
    ("grey89", 0xE3E3E3),
    ("grey90", 0xE5E5E5),
    ("grey91", 0xE8E8E8),
    ("grey92", 0xEBEBEB),
    ("grey93", 0xEDEDED),
    ("grey94", 0xF0F0F0),
    ("grey95", 0xF2F2F2),
    ("grey96", 0xF5F5F5),
    ("grey97", 0xF7F7F7),
    ("grey98", 0xFAFAFA),
    ("grey99", 0xFCFCFC),
    ("grey100", 0xFFFFFF),
    ("honeydew", 0xF0FFF0),
    ("honeydew1", 0xF0FFF0),
    ("honeydew2", 0xE0EEE0),
    ("honeydew3", 0xC1CDC1),
    ("honeydew4", 0x838B83),
    ("hotpink", 0xB469FF),
    ("hotpink1", 0xB46EFF),
    ("hotpink2", 0xA76AEE),
    ("hotpink3", 0x9060CD),
    ("hotpink4", 0x623A8B),
    ("indianred", 0x5C5CCD),
    ("indianred1", 0x6A6AFF),
    ("indianred2", 0x6363EE),
    ("indianred3", 0x5555CD),
    ("indianred4", 0x3A3A8B),
    ("ivory", 0xF0FFFF),
    ("ivory1", 0xF0FFFF),
    ("ivory2", 0xE0EEEE),
    ("ivory3", 0xC1CDCD),
    ("ivory4", 0x838B8B),
    ("khaki", 0x8CE6F0),
    ("khaki1", 0x8FF6FF),
    ("khaki2", 0x85E6EE),
    ("khaki3", 0x73C6CD),
    ("khaki4", 0x4E868B),
    ("lavender", 0xFAE6E6),
    ("lavenderblush", 0xF5F0FF),
    ("lavenderblush1", 0xF5F0FF),
    ("lavenderblush2", 0xE5E0EE),
    ("lavenderblush3", 0xC5C1CD),
    ("lavenderblush4", 0x86838B),
    ("lawngreen", 0x00FC7C),
    ("lemonchiffon", 0xCDFAFF),
    ("lemonchiffon1", 0xCDFAFF),
    ("lemonchiffon2", 0xBFE9EE),
    ("lemonchiffon3", 0xA5C9CD),
    ("lemonchiffon4", 0x70898B),
    ("lightblue", 0xE6D8AD),
    ("lightblue1", 0xFFEFBF),
    ("lightblue2", 0xEEDFB2),
    ("lightblue3", 0xCDC09A),
    ("lightblue4", 0x8B8368),
    ("lightcoral", 0x8080F0),
    ("lightcyan", 0xFFFFE0),
    ("lightcyan1", 0xFFFFE0),
    ("lightcyan2", 0xEEEED1),
    ("lightcyan3", 0xCDCDB4),
    ("lightcyan4", 0x8B8B7A),
    ("lightgoldenrod", 0x82DDEE),
    ("lightgoldenrod1", 0x8BECFF),
    ("lightgoldenrod2", 0x82DCEE),
    ("lightgoldenrod3", 0x70BECD),
    ("lightgoldenrod4", 0x4C818B),
    ("lightgoldenrodyellow", 0xD2FAFA),
    ("lightgray", 0xD3D3D3),
    ("lightgreen", 0x90EE90),
    ("lightgrey", 0xD3D3D3),
    ("lightpink", 0xC1B6FF),
    ("lightpink1", 0xB9AEFF),
    ("lightpink2", 0xADA2EE),
    ("lightpink3", 0x958CCD),
    ("lightpink4", 0x655F8B),
    ("lightsalmon", 0x7AA0FF),
    ("lightsalmon1", 0x7AA0FF),
    ("lightsalmon2", 0x7295EE),
    ("lightsalmon3", 0x6281CD),
    ("lightsalmon4", 0x42578B),
    ("lightseagreen", 0xAAB220),
    ("lightskyblue", 0xFACE87),
    ("lightskyblue1", 0xFFE2B0),
    ("lightskyblue2", 0xEED3A4),
    ("lightskyblue3", 0xCDB68D),
    ("lightskyblue4", 0x8B7B60),
    ("lightslateblue", 0xFF7084),
    ("lightslategray", 0x998877),
    ("lightslategrey", 0x998877),
    ("lightsteelblue", 0xDEC4B0),
    ("lightsteelblue1", 0xFFE1CA),
    ("lightsteelblue2", 0xEED2BC),
    ("lightsteelblue3", 0xCDB5A2),
    ("lightsteelblue4", 0x8B7B6E),
    ("lightyellow", 0xE0FFFF),
    ("lightyellow1", 0xE0FFFF),
    ("lightyellow2", 0xD1EEEE),
    ("lightyellow3", 0xB4CDCD),
    ("lightyellow4", 0x7A8B8B),
    ("limegreen", 0x32CD32),
    ("linen", 0xE6F0FA),
    ("magenta", 0xFF00FF),
    ("magenta1", 0xFF00FF),
    ("magenta2", 0xEE00EE),
    ("magenta3", 0xCD00CD),
    ("magenta4", 0x8B008B),
    ("maroon", 0x6030B0),
    ("maroon1", 0xB334FF),
    ("maroon2", 0xA730EE),
    ("maroon3", 0x9029CD),
    ("maroon4", 0x621C8B),
    ("mediumaquamarine", 0xAACD66),
    ("mediumblue", 0xCD0000),
    ("mediumorchid", 0xD355BA),
    ("mediumorchid1", 0xFF66E0),
    ("mediumorchid2", 0xEE5FD1),
    ("mediumorchid3", 0xCD52B4),
    ("mediumorchid4", 0x8B377A),
    ("mediumpurple", 0xDB7093),
    ("mediumpurple1", 0xFF82AB),
    ("mediumpurple2", 0xEE799F),
    ("mediumpurple3", 0xCD6889),
    ("mediumpurple4", 0x8B475D),
    ("mediumseagreen", 0x71B33C),
    ("mediumslateblue", 0xEE687B),
    ("mediumspringgreen", 0x9AFA00),
    ("mediumturquoise", 0xCCD148),
    ("mediumvioletred", 0x8515C7),
    ("midnightblue", 0x701919),
    ("mintcream", 0xFAFFF5),
    ("mistyrose", 0xE1E4FF),
    ("mistyrose1", 0xE1E4FF),
    ("mistyrose2", 0xD2D5EE),
    ("mistyrose3", 0xB5B7CD),
    ("mistyrose4", 0x7B7D8B),
    ("moccasin", 0xB5E4FF),
    ("navajowhite", 0xADDEFF),
    ("navajowhite1", 0xADDEFF),
    ("navajowhite2", 0xA1CFEE),
    ("navajowhite3", 0x8BB3CD),
    ("navajowhite4", 0x5E798B),
    ("navy", 0x800000),
    ("navyblue", 0x800000),
    ("oldlace", 0xE6F5FD),
    ("olivedrab", 0x238E6B),
    ("olivedrab1", 0x3EFFC0),
    ("olivedrab2", 0x3AEEB3),
    ("olivedrab3", 0x32CD9A),
    ("olivedrab4", 0x228B69),
    ("orange", 0x00A5FF),
    ("orange1", 0x00A5FF),
    ("orange2", 0x009AEE),
    ("orange3", 0x0085CD),
    ("orange4", 0x005A8B),
    ("orangered", 0x0045FF),
    ("orangered1", 0x0045FF),
    ("orangered2", 0x0040EE),
    ("orangered3", 0x0037CD),
    ("orangered4", 0x00258B),
    ("orchid", 0xD670DA),
    ("orchid1", 0xFA83FF),
    ("orchid2", 0xE97AEE),
    ("orchid3", 0xC969CD),
    ("orchid4", 0x89478B),
    ("palegoldenrod", 0xAAE8EE),
    ("palegreen", 0x98FB98),
    ("palegreen1", 0x9AFF9A),
    ("palegreen2", 0x90EE90),
    ("palegreen3", 0x7CCD7C),
    ("palegreen4", 0x548B54),
    ("paleturquoise", 0xEEEEAF),
    ("paleturquoise1", 0xFFFFBB),
    ("paleturquoise2", 0xEEEEAE),
    ("paleturquoise3", 0xCDCD96),
    ("paleturquoise4", 0x8B8B66),
    ("palevioletred", 0x9370DB),
    ("palevioletred1", 0xAB82FF),
    ("palevioletred2", 0x9F79EE),
    ("palevioletred3", 0x8968CD),
    ("palevioletred4", 0x5D478B),
    ("papayawhip", 0xD5EFFF),
    ("peachpuff", 0xB9DAFF),
    ("peachpuff1", 0xB9DAFF),
    ("peachpuff2", 0xADCBEE),
    ("peachpuff3", 0x95AFCD),
    ("peachpuff4", 0x65778B),
    ("peru", 0x3F85CD),
    ("pink", 0xCBC0FF),
    ("pink1", 0xC5B5FF),
    ("pink2", 0xB8A9EE),
    ("pink3", 0x9E91CD),
    ("pink4", 0x6C638B),
    ("plum", 0xDDA0DD),
    ("plum1", 0xFFBBFF),
    ("plum2", 0xEEAEEE),
    ("plum3", 0xCD96CD),
    ("plum4", 0x8B668B),
    ("powderblue", 0xE6E0B0),
    ("purple", 0xF020A0),
    ("purple1", 0xFF309B),
    ("purple2", 0xEE2C91),
    ("purple3", 0xCD267D),
    ("purple4", 0x8B1A55),
    ("red", 0x0000FF),
    ("red1", 0x0000FF),
    ("red2", 0x0000EE),
    ("red3", 0x0000CD),
    ("red4", 0x00008B),
    ("rosybrown", 0x8F8FBC),
    ("rosybrown1", 0xC1C1FF),
    ("rosybrown2", 0xB4B4EE),
    ("rosybrown3", 0x9B9BCD),
    ("rosybrown4", 0x69698B),
    ("royalblue", 0xE16941),
    ("royalblue1", 0xFF7648),
    ("royalblue2", 0xEE6E43),
    ("royalblue3", 0xCD5F3A),
    ("royalblue4", 0x8B4027),
    ("saddlebrown", 0x13458B),
    ("salmon", 0x7280FA),
    ("salmon1", 0x698CFF),
    ("salmon2", 0x6282EE),
    ("salmon3", 0x5470CD),
    ("salmon4", 0x394C8B),
    ("sandybrown", 0x60A4F4),
    ("seagreen", 0x578B2E),
    ("seagreen1", 0x9FFF54),
    ("seagreen2", 0x94EE4E),
    ("seagreen3", 0x80CD43),
    ("seagreen4", 0x578B2E),
    ("seashell", 0xEEF5FF),
    ("seashell1", 0xEEF5FF),
    ("seashell2", 0xDEE5EE),
    ("seashell3", 0xBFC5CD),
    ("seashell4", 0x82868B),
    ("sienna", 0x2D52A0),
    ("sienna1", 0x4782FF),
    ("sienna2", 0x4279EE),
    ("sienna3", 0x3968CD),
    ("sienna4", 0x26478B),
    ("skyblue", 0xEBCE87),
    ("skyblue1", 0xFFCE87),
    ("skyblue2", 0xEEC07E),
    ("skyblue3", 0xCDA66C),
    ("skyblue4", 0x8B704A),
    ("slateblue", 0xCD5A6A),
    ("slateblue1", 0xFF6F83),
    ("slateblue2", 0xEE677A),
    ("slateblue3", 0xCD5969),
    ("slateblue4", 0x8B3C47),
    ("slategray", 0x908070),
    ("slategray1", 0xFFE2C6),
    ("slategray2", 0xEED3B9),
    ("slategray3", 0xCDB69F),
    ("slategray4", 0x8B7B6C),
    ("slategrey", 0x908070),
    ("snow", 0xFAFAFF),
    ("snow1", 0xFAFAFF),
    ("snow2", 0xE9E9EE),
    ("snow3", 0xC9C9CD),
    ("snow4", 0x89898B),
    ("springgreen", 0x7FFF00),
    ("springgreen1", 0x7FFF00),
    ("springgreen2", 0x76EE00),
    ("springgreen3", 0x66CD00),
    ("springgreen4", 0x458B00),
    ("steelblue", 0xB48246),
    ("steelblue1", 0xFFB863),
    ("steelblue2", 0xEEAC5C),
    ("steelblue3", 0xCD944F),
    ("steelblue4", 0x8B6436),
    ("tan", 0x8CB4D2),
    ("tan1", 0x4FA5FF),
    ("tan2", 0x499AEE),
    ("tan3", 0x3F85CD),
    ("tan4", 0x2B5A8B),
    ("thistle", 0xD8BFD8),
    ("thistle1", 0xFFE1FF),
    ("thistle2", 0xEED2EE),
    ("thistle3", 0xCDB5CD),
    ("thistle4", 0x8B7B8B),
    ("tomato", 0x4763FF),
    ("tomato1", 0x4763FF),
    ("tomato2", 0x425CEE),
    ("tomato3", 0x394FCD),
    ("tomato4", 0x26368B),
    ("turquoise", 0xD0E040),
    ("turquoise1", 0xFFF500),
    ("turquoise2", 0xEEE500),
    ("turquoise3", 0xCDC500),
    ("turquoise4", 0x8B8600),
    ("violet", 0xEE82EE),
    ("violetred", 0x9020D0),
    ("violetred1", 0x963EFF),
    ("violetred2", 0x8C3AEE),
    ("violetred3", 0x7832CD),
    ("violetred4", 0x52228B),
    ("wheat", 0xB3DEF5),
    ("wheat1", 0xBAE7FF),
    ("wheat2", 0xAED8EE),
    ("wheat3", 0x96BACD),
    ("wheat4", 0x667E8B),
    ("whitesmoke", 0xF5F5F5),
    ("yellow", 0x00FFFF),
    ("yellow1", 0x00FFFF),
    ("yellow2", 0x00EEEE),
    ("yellow3", 0x00CDCD),
    ("yellow4", 0x008B8B),
    ("yellowgreen", 0x32CD9A),
];

fn unsupported_message() -> crate::boundary::Unwind {
    error("graphics devices are not supported by ry yet")
}

macro_rules! unsupported {
    ($($name:ident),* $(,)?) => {
        $(
            /// Needs a graphics device, which ry does not have yet.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn $name() {
                raise(unsupported_message())
            }
        )*
    };
}

unsupported!(
    GEaddDevice,
    GEaddDevice2,
    GEaddDevice2f,
    GECap,
    GECircle,
    GEcheckState,
    GEcontourLines,
    GEcopyDisplayList,
    GEcreateDD,
    GEcreateDevDesc,
    GEcreateSnapshot,
    GEcurrentDevice,
    GEdestroyDevDesc,
    GEdeviceDirty,
    GEdeviceNumber,
    GEdirtyDevice,
    GEExpressionHeight,
    GEExpressionMetric,
    GEExpressionWidth,
    GEFill,
    GEFillStroke,
    GEfreeDD,
    GEgetDevice,
    GEGlyph,
    GEhandleEvent,
    GEinitDisplayList,
    GEkillDevice,
    GELine,
    GEMathText,
    GEMetricInfo,
    GEMode,
    GENewPage,
    GEonExit,
    GEPath,
    GEplayDisplayList,
    GEplaySnapshot,
    GEPolygon,
    GEPolyline,
    GEPretty,
    GERaster,
    GErecordGraphicOperation,
    GErecording,
    GERect,
    GEregisterSystem,
    GEregisterWithDevice,
    GESetClip,
    GEStrHeight,
    GEStrMetric,
    GEStrWidth,
    GEStroke,
    GESymbol,
    GEsystemState,
    GEText,
    GEunregisterSystem,
    GEXspline,
    GAxisPars,
    CreateAtVector,
    Rf_eval_with_gd,
    desc2GEDesc,
    fromDeviceHeight,
    fromDeviceWidth,
    fromDeviceX,
    fromDeviceY,
    GEfromDeviceHeight,
    GEfromDeviceWidth,
    GEfromDeviceX,
    GEfromDeviceY,
    toDeviceHeight,
    toDeviceWidth,
    toDeviceX,
    toDeviceY,
    GEtoDeviceHeight,
    GEtoDeviceWidth,
    GEtoDeviceX,
    GEtoDeviceY,
    R_GE_clipPathFillRule,
    R_GE_glyphColour,
    R_GE_glyphFont,
    R_GE_glyphFontFamily,
    R_GE_glyphFontFile,
    R_GE_glyphFontIndex,
    R_GE_glyphFontNumVar,
    R_GE_glyphFontPSname,
    R_GE_glyphFontStyle,
    R_GE_glyphFontVarAxis,
    R_GE_glyphFontVarFormatted,
    R_GE_glyphFontVarValue,
    R_GE_glyphFontWeight,
    R_GE_glyphID,
    R_GE_glyphInfoFonts,
    R_GE_glyphInfoGlyphs,
    R_GE_glyphRotation,
    R_GE_glyphSize,
    R_GE_glyphX,
    R_GE_glyphY,
    R_GE_hasGlyphRotation,
    R_GE_isPattern,
    R_GE_linearGradientColour,
    R_GE_linearGradientExtend,
    R_GE_linearGradientNumStops,
    R_GE_linearGradientStop,
    R_GE_linearGradientX1,
    R_GE_linearGradientX2,
    R_GE_linearGradientY1,
    R_GE_linearGradientY2,
    R_GE_maskType,
    R_GE_patternType,
    R_GE_radialGradientCX1,
    R_GE_radialGradientCX2,
    R_GE_radialGradientCY1,
    R_GE_radialGradientCY2,
    R_GE_radialGradientColour,
    R_GE_radialGradientExtend,
    R_GE_radialGradientNumStops,
    R_GE_radialGradientR1,
    R_GE_radialGradientR2,
    R_GE_radialGradientStop,
    R_GE_rasterInterpolate,
    R_GE_rasterResizeForRotation,
    R_GE_rasterRotate,
    R_GE_rasterRotatedOffset,
    R_GE_rasterRotatedSize,
    R_GE_rasterScale,
    R_GE_tilingPatternExtend,
    R_GE_tilingPatternFunction,
    R_GE_tilingPatternHeight,
    R_GE_tilingPatternWidth,
    R_GE_tilingPatternX,
    R_GE_tilingPatternY,
    GE_LENDget,
    GE_LENDpar,
    GE_LJOINget,
    GE_LJOINpar,
    GE_LTYget,
    GE_LTYpar,
    GEstring_to_pch,
    killDevice,
    nextDevice,
    prevDevice,
    selectDevice,
    ndevNumber,
    NewFrameConfirm,
    doIdle,
    doKeybd,
    doMouseEvent,
    doesIdle,
    AdobeSymbol2utf8,
    utf8toAdobeSymbol,
    utf8ToLatin1AdobeSymbol2utf8,
    Rf_KillAllDevices,
);

#[unsafe(no_mangle)]
pub extern "C" fn R_GE_getVersion() -> c_int {
    GE_VERSION
}

/// `R_GE_checkVersionOrDie(version)`: a device built for another engine version cannot run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_checkVersionOrDie(version: c_int) {
    if version != GE_VERSION {
        raise(error(format!(
            "Graphics API version mismatch: the device expects version {version}, ry provides \
             {GE_VERSION}"
        )));
    }
}

/// `R_CheckDeviceAvailable()`: there is no device list to add a device to.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_CheckDeviceAvailable() {
    raise(unsupported_message())
}

#[unsafe(no_mangle)]
pub extern "C" fn R_CheckDeviceAvailableBool() -> crate::Rboolean {
    0
}

/// Only the null device exists.
#[unsafe(no_mangle)]
pub extern "C" fn NoDevices() -> c_int {
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn NumDevices() -> c_int {
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn curDevice() -> c_int {
    0
}

/// ry's runtime always runs in a UTF-8 locale.
#[unsafe(no_mangle)]
pub static mbcslocale: crate::Global<crate::Rboolean> = crate::Global::new(1);

/// `Rf_ucstoutf8(s, c)`: writes the UTF-8 encoding of code point `c` into `s` (at least seven
/// bytes) with a terminating NUL, and returns `s`. An invalid code point writes the empty
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_ucstoutf8(buffer: *mut c_char, code: c_uint) -> *mut c_char {
    let mut bytes = [0u8; 4];
    let encoded = char::from_u32(code).map_or(&[][..], |character| {
        character.encode_utf8(&mut bytes).as_bytes()
    });
    std::ptr::copy_nonoverlapping(encoded.as_ptr(), buffer as *mut u8, encoded.len());
    *buffer.add(encoded.len()) = 0;
    buffer
}

/// Removes the private-use code points some fonts map symbols to. Without a symbol font there
/// is nothing to remap, so the text is copied.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn utf8Toutf8NoPUA(input: *const c_char) -> *const c_char {
    input
}

fn hex_digit(byte: u8) -> Option<u32> {
    (byte as char).to_digit(16)
}

/// Parses a colour name, `#RRGGBB[AA]`, `#RGB[A]`, `transparent`, `NA`, or a palette index.
pub fn parse_color(text: &str) -> Option<RColor> {
    let bytes = text.as_bytes();
    if let Some(hex) = text.strip_prefix('#') {
        let digits: Option<Vec<u32>> = hex.bytes().map(hex_digit).collect();
        let digits = digits?;
        let channel = |high: u32, low: u32| high * 16 + low;
        return Some(match digits.len() {
            6 | 8 => {
                let red = channel(digits[0], digits[1]);
                let green = channel(digits[2], digits[3]);
                let blue = channel(digits[4], digits[5]);
                let alpha = if digits.len() == 8 {
                    channel(digits[6], digits[7])
                } else {
                    255
                };
                red | green << 8 | blue << 16 | alpha << 24
            }
            3 | 4 => {
                let alpha = if digits.len() == 4 { digits[3] * 17 } else { 255 };
                digits[0] * 17 | (digits[1] * 17) << 8 | (digits[2] * 17) << 16 | alpha << 24
            }
            _ => return None,
        });
    }
    if text == "NA" || text.eq_ignore_ascii_case("transparent") {
        return Some(TRANSPARENT_WHITE);
    }
    if !bytes.is_empty() && bytes.iter().all(u8::is_ascii_digit) {
        let index: usize = text.parse().ok()?;
        return palette_color(index as i64);
    }
    let wanted: String = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    COLOR_NAMES
        .iter()
        .find(|(name, _)| *name == wanted)
        .map(|&(_, color)| color | OPAQUE)
}

/// A palette index: 0 is the transparent background, and positive indices cycle through the
/// palette.
fn palette_color(index: i64) -> Option<RColor> {
    if index < 0 {
        return None;
    }
    if index == 0 {
        return Some(TRANSPARENT_WHITE);
    }
    Some(PALETTE[(index as usize - 1) % PALETTE.len()])
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_str2col(text: *const c_char) -> RColor {
    enter(|| {
        let text = String::from_utf8_lossy(c_bytes(text));
        parse_color(&text).ok_or_else(|| error(format!("invalid color name '{text}'")))
    })
}

unsafe fn color_at(value: Sexp, index: c_int, background: RColor) -> ApiResult<RColor> {
    let length = xlength(value)?;
    if length == 0 {
        return Err(error("invalid color specification"));
    }
    let index = index as usize % length as usize;
    match value.kind() {
        SexpType::String => {
            let element = value.element(index);
            if element == runtime().heap.known().na_string {
                return Ok(TRANSPARENT_WHITE);
            }
            let text = utf8_text(element)?;
            if text == "0" {
                return Ok(background);
            }
            parse_color(&text).ok_or_else(|| error(format!("invalid color name '{text}'")))
        }
        SexpType::Logical | SexpType::Integer => {
            let number = int_elt(value, index)?;
            if number == NA_INTEGER {
                return Ok(TRANSPARENT_WHITE);
            }
            if number == 0 {
                return Ok(background);
            }
            palette_color(number as i64).ok_or_else(|| error("numerical color values must be >= 0, found -1"))
        }
        SexpType::Real => {
            let number = real_elt(value, index)?;
            if is_na_real(number) || number.is_nan() {
                return Ok(TRANSPARENT_WHITE);
            }
            if number == 0.0 {
                return Ok(background);
            }
            palette_color(number as i64).ok_or_else(|| error("numerical color values must be >= 0"))
        }
        _ => Err(error("invalid color specification")),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn RGBpar(value: Sexp, index: c_int) -> RColor {
    enter(|| color_at(value, index, TRANSPARENT_WHITE))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_RGBpar(value: Sexp, index: c_int) -> RColor {
    enter(|| color_at(value, index, TRANSPARENT_WHITE))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn RGBpar3(value: Sexp, index: c_int, background: RColor) -> RColor {
    enter(|| color_at(value, index, background))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_RGBpar3(value: Sexp, index: c_int, background: RColor) -> RColor {
    enter(|| color_at(value, index, background))
}

/// `col2name(color)`: the colour's name when an opaque colour has one, `transparent`, or a hex
/// string, in a static buffer as GNU R returns it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn col2name(color: RColor) -> *const c_char {
    static BUFFER: crate::Global<[u8; 32]> = crate::Global::new([0; 32]);
    let alpha = color >> 24;
    let text = if alpha == 255 {
        COLOR_NAMES
            .iter()
            .find(|(_, value)| *value == color & 0x00FF_FFFF)
            .map(|(name, _)| (*name).to_owned())
            .unwrap_or_else(|| {
                format!(
                    "#{:02X}{:02X}{:02X}",
                    color & 0xFF,
                    (color >> 8) & 0xFF,
                    (color >> 16) & 0xFF
                )
            })
    } else if alpha == 0 {
        "transparent".to_owned()
    } else {
        format!(
            "#{:02X}{:02X}{:02X}{:02X}",
            color & 0xFF,
            (color >> 8) & 0xFF,
            (color >> 16) & 0xFF,
            alpha
        )
    };
    let buffer = &mut *BUFFER.as_ptr();
    let length = text.len().min(buffer.len() - 1);
    buffer[..length].copy_from_slice(&text.as_bytes()[..length]);
    buffer[length] = 0;
    buffer.as_ptr() as *const c_char
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_col2name(color: RColor) -> *const c_char {
    col2name(color)
}

//! The graphics engine and device API: the C entry points of R's graphics engine, over the
//! engine in `interp::graphics_engine`, so that compiled graphics systems (grid) and devices
//! (ragg) run unchanged.

use {
    crate::{
        Rboolean,
        attributes::{get_attrib, inherits},
        boundary::{ApiResult, enter, error, raise},
        globals::R_NamesSymbol,
        runtime,
        strings::{c_bytes, utf8_text},
        vectors::{int_elt, real_elt, sexp_elt, xlength},
    },
    heap::object::{NA_INTEGER, Sexp, SexpType, is_na_real},
    interp::{
        graphics,
        graphics_engine::{self, DevDesc, GEContext, GEDevDesc, SystemCallback, Unit},
        values,
    },
    std::ffi::{c_char, c_int, c_uint, c_void},
};

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

/// `GEregisterSystem(callback, &index)`: registers a graphics system such as grid with every
/// open device and every device opened later.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEregisterSystem(callback: Option<SystemCallback>, index: *mut c_int) {
    enter(|| {
        let callback = callback.ok_or_else(|| error("invalid graphics system callback"))?;
        graphics_engine::register_system(callback, |claimed| {
            if !index.is_null() {
                *index = claimed as c_int;
            }
        })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEunregisterSystem(index: c_int) {
    enter(|| graphics_engine::unregister_system(index))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEregisterWithDevice(dd: *mut GEDevDesc) {
    enter(|| graphics_engine::register_with_device(dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEsystemState(dd: *mut GEDevDesc, index: c_int) -> *mut c_void {
    match usize::try_from(index) {
        Ok(index) => graphics_engine::system_state(dd, index),
        Err(_) => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEhandleEvent(event: c_int, dev: *mut DevDesc, data: Sexp) -> Sexp {
    enter(|| graphics_engine::handle_event(event, dev, data))
}

/// `GEcreateDD()`: a zeroed device description for a device to fill in. GNU R allocates it
/// with `calloc`, and devices free it with `free` when they fail to open, so this does too.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEcreateDD() -> *mut DevDesc {
    let dev = libc::calloc(1, std::mem::size_of::<DevDesc>()).cast::<DevDesc>();
    if dev.is_null() {
        raise(error(
            "unable to allocate memory for the device description",
        ));
    }
    dev
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEfreeDD(dev: *mut DevDesc) {
    libc::free(dev.cast());
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEcreateDevDesc(dev: *mut DevDesc) -> *mut GEDevDesc {
    enter(|| graphics_engine::create_device(dev))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEdestroyDevDesc(dd: *mut GEDevDesc) {
    enter(|| graphics_engine::destroy_device(dd))
}

/// `GEaddDevice(dd)`: adds a device named by the `.Device` variable its opener set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEaddDevice(dd: *mut GEDevDesc) {
    enter(|| graphics_engine::add_device_from_dot_device(dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEaddDevice2(dd: *mut GEDevDesc, name: *const c_char) {
    enter(|| graphics_engine::add_device_named(dd, &text_of(name), None))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEaddDevice2f(
    dd: *mut GEDevDesc,
    name: *const c_char,
    file: *const c_char,
) {
    enter(|| {
        let file = (!file.is_null()).then(|| text_of(file));
        graphics_engine::add_device_named(dd, &text_of(name), file.as_deref())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEkillDevice(dd: *mut GEDevDesc) {
    enter(|| graphics_engine::kill(dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEcurrentDevice() -> *mut GEDevDesc {
    enter(|| graphics_engine::current_device())
}

#[unsafe(no_mangle)]
pub extern "C" fn GEgetDevice(number: c_int) -> *mut GEDevDesc {
    match usize::try_from(number) {
        Ok(number) if number < graphics_engine::MAX_DEVICES => graphics_engine::device(number),
        _ => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn GEdeviceNumber(dd: *mut GEDevDesc) -> c_int {
    graphics_engine::device_number(dd) as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_desc2GEDesc(dev: *mut DevDesc) -> *mut GEDevDesc {
    graphics_engine::desc_to_device(dev)
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_ndevNumber(dev: *mut DevDesc) -> c_int {
    graphics_engine::device_number(graphics_engine::desc_to_device(dev)) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_killDevice(number: c_int) {
    if let Ok(number) = usize::try_from(number) {
        enter(|| graphics_engine::kill_device(number))
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_KillAllDevices() {
    enter(|| graphics_engine::kill_all_devices())
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_nextDevice(from: c_int) -> c_int {
    graphics_engine::next_device(usize::try_from(from).unwrap_or(0)) as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_prevDevice(from: c_int) -> c_int {
    graphics_engine::previous_device(usize::try_from(from).unwrap_or(0)) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_selectDevice(number: c_int) -> c_int {
    let number = usize::try_from(number).unwrap_or(0);
    enter(|| Ok(graphics_engine::select_device(number)? as c_int))
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_NoDevices() -> c_int {
    c_int::from(graphics_engine::no_devices())
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_NumDevices() -> c_int {
    graphics_engine::num_devices() as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_curDevice() -> c_int {
    graphics_engine::current_number() as c_int
}

/// `R_CheckDeviceAvailable()`: an error when all 63 device slots are taken.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_CheckDeviceAvailable() {
    enter(graphics_engine::check_device_available)
}

#[unsafe(no_mangle)]
pub extern "C" fn R_CheckDeviceAvailableBool() -> crate::Rboolean {
    crate::Rboolean::from(graphics_engine::device_available())
}

/// `NewFrameConfirm(dev)`: asks before a new page. A device that confirms itself does so, and
/// ry's console does not prompt otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_NewFrameConfirm(dev: *mut DevDesc) {
    if let Some(confirm) = (*dev).new_frame_confirm {
        confirm(dev);
    }
}

/// Graphics events (`getGraphicsEvent`) are not supported, so a device's events are dropped.
#[unsafe(no_mangle)]
pub extern "C" fn Rf_doMouseEvent(
    _dev: *mut DevDesc,
    _event: c_int,
    _buttons: c_int,
    _x: f64,
    _y: f64,
) {
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_doKeybd(_dev: *mut DevDesc, _key: c_int, _name: *const c_char) {}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_doIdle(_dev: *mut DevDesc) {}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_doesIdle(_dev: *mut DevDesc) -> crate::Rboolean {
    0
}

/// `AdobeSymbol2utf8(out, in, nwork, usePUA)`: ry does not map the Adobe Symbol encoding, so
/// the text is copied unchanged.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_AdobeSymbol2utf8(
    out: *mut c_char,
    input: *const c_char,
    work: usize,
    _use_pua: crate::Rboolean,
) -> *mut c_void {
    if work > 0 {
        let bytes = c_bytes(input);
        let length = bytes.len().min(work - 1);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.cast::<u8>(), length);
        *out.add(length) = 0;
    }
    out.cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_utf8toAdobeSymbol(out: *mut c_char, input: *const c_char) -> c_int {
    let bytes = c_bytes(input);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.cast::<u8>(), bytes.len());
    *out.add(bytes.len()) = 0;
    bytes.len() as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn Rf_utf8ToLatin1AdobeSymbol2utf8(
    input: *const c_char,
    _use_pua: crate::Rboolean,
) -> *const c_char {
    input
}

/// `eval_with_gd(expr, rho, dd)`: evaluates `expr`. GNU R's version only differs in which
/// device a graphics error names.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_eval_with_gd(expression: Sexp, env: Sexp, _dd: *mut GEDevDesc) -> Sexp {
    crate::eval::Rf_eval(expression, env)
}

macro_rules! units {
    ($($from:ident, $ge_from:ident, $to:ident, $ge_to:ident => $engine_from:ident, $engine_to:ident;)*) => {
        $(
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn $from(value: f64, to: c_int, dd: *mut GEDevDesc) -> f64 {
                graphics_engine::$engine_from(value, Unit::from_code(to), dd)
            }

            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn $ge_from(value: f64, to: c_int, dd: *mut GEDevDesc) -> f64 {
                graphics_engine::$engine_from(value, Unit::from_code(to), dd)
            }

            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn $to(value: f64, from: c_int, dd: *mut GEDevDesc) -> f64 {
                graphics_engine::$engine_to(value, Unit::from_code(from), dd)
            }

            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn $ge_to(value: f64, from: c_int, dd: *mut GEDevDesc) -> f64 {
                graphics_engine::$engine_to(value, Unit::from_code(from), dd)
            }
        )*
    };
}

units! {
    fromDeviceX, GEfromDeviceX, toDeviceX, GEtoDeviceX => from_device_x, to_device_x;
    fromDeviceY, GEfromDeviceY, toDeviceY, GEtoDeviceY => from_device_y, to_device_y;
    fromDeviceWidth, GEfromDeviceWidth, toDeviceWidth, GEtoDeviceWidth
        => from_device_width, to_device_width;
    fromDeviceHeight, GEfromDeviceHeight, toDeviceHeight, GEtoDeviceHeight
        => from_device_height, to_device_height;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GE_LENDpar(value: Sexp, index: c_int) -> c_int {
    enter(|| graphics_engine::lend_par(value, index_of(index)))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GE_LENDget(lend: c_int) -> Sexp {
    enter(|| values::string_vector(&[graphics_engine::lend_name(lend)?]))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GE_LJOINpar(value: Sexp, index: c_int) -> c_int {
    enter(|| graphics_engine::ljoin_par(value, index_of(index)))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GE_LJOINget(ljoin: c_int) -> Sexp {
    enter(|| values::string_vector(&[graphics_engine::ljoin_name(ljoin)?]))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GE_LTYpar(value: Sexp, index: c_int) -> c_uint {
    enter(|| Ok(graphics_engine::lty_par(value, index_of(index))? as c_uint))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GE_LTYget(lty: c_uint) -> Sexp {
    enter(|| values::string_vector(&[graphics_engine::lty_name(lty as c_int)]))
}

/// `GEstring_to_pch(chr)`: the plotting symbol a one-character string names.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEstring_to_pch(text: Sexp) -> c_int {
    enter(|| {
        if text == runtime().heap.known().na_string {
            return graphics_engine::string_to_pch(None);
        }
        graphics_engine::string_to_pch(Some(&utf8_text(text)?))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GESetClip(x1: f64, y1: f64, x2: f64, y2: f64, dd: *mut GEDevDesc) {
    enter(|| graphics_engine::set_clip(x1, y1, x2, y2, dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GENewPage(gc: *mut GEContext, dd: *mut GEDevDesc) {
    enter(|| graphics_engine::new_page(&*gc, dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GELine(
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    gc: *mut GEContext,
    dd: *mut GEDevDesc,
) {
    enter(|| graphics_engine::line(x1, y1, x2, y2, &*gc, dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEPolyline(
    count: c_int,
    x: *mut f64,
    y: *mut f64,
    gc: *mut GEContext,
    dd: *mut GEDevDesc,
) {
    enter(|| graphics_engine::polyline(slice(x, count), slice(y, count), &*gc, dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEPolygon(
    count: c_int,
    x: *mut f64,
    y: *mut f64,
    gc: *mut GEContext,
    dd: *mut GEDevDesc,
) {
    enter(|| graphics_engine::polygon(slice(x, count), slice(y, count), &mut *gc, dd))
}

/// `GEXspline(n, x, y, s, open, repEnds, draw, gc, dd)`: the X-spline's points in device
/// units as `list(x, y)`, drawn first when `draw` is set.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn GEXspline(
    count: c_int,
    x: *mut f64,
    y: *mut f64,
    shape: *mut f64,
    open: crate::Rboolean,
    repeat_ends: crate::Rboolean,
    draw: crate::Rboolean,
    gc: *mut GEContext,
    dd: *mut GEDevDesc,
) -> Sexp {
    enter(|| {
        let (xs, ys) = graphics_engine::xspline(
            slice(x, count),
            slice(y, count),
            slice(shape, count),
            open != 0,
            repeat_ends != 0,
            draw != 0,
            &mut *gc,
            dd,
        )?;
        let points = [values::real_vector(&xs)?, values::real_vector(&ys)?];
        values::list_vector(&points)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GECircle(
    x: f64,
    y: f64,
    radius: f64,
    gc: *mut GEContext,
    dd: *mut GEDevDesc,
) {
    enter(|| graphics_engine::circle(x, y, radius, &mut *gc, dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GERect(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    gc: *mut GEContext,
    dd: *mut GEDevDesc,
) {
    enter(|| graphics_engine::rect(x0, y0, x1, y1, &mut *gc, dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEPath(
    x: *mut f64,
    y: *mut f64,
    polygons: c_int,
    counts: *mut c_int,
    winding: crate::Rboolean,
    gc: *mut GEContext,
    dd: *mut GEDevDesc,
) {
    enter(|| {
        let counts = slice(counts, polygons);
        let total: c_int = counts.iter().sum();
        graphics_engine::path(
            slice(x, total),
            slice(y, total),
            counts,
            winding != 0,
            &mut *gc,
            dd,
        )
    })
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn GERaster(
    raster: *mut c_uint,
    columns: c_int,
    rows: c_int,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    angle: f64,
    interpolate: crate::Rboolean,
    gc: *mut GEContext,
    dd: *mut GEDevDesc,
) {
    enter(|| {
        let length = columns.max(0) as usize * rows.max(0) as usize;
        let pixels = if raster.is_null() || length == 0 {
            &mut []
        } else {
            std::slice::from_raw_parts_mut(raster, length)
        };
        graphics_engine::raster(
            pixels,
            columns,
            rows,
            x,
            y,
            width,
            height,
            angle,
            interpolate != 0,
            &*gc,
            dd,
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GECap(dd: *mut GEDevDesc) -> Sexp {
    enter(|| graphics_engine::capture(dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEMode(mode: c_int, dd: *mut GEDevDesc) {
    enter(|| graphics_engine::mode(mode, dd))
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn GEText(
    x: f64,
    y: f64,
    text: *const c_char,
    encoding: c_int,
    xc: f64,
    yc: f64,
    rotation: f64,
    gc: *mut GEContext,
    dd: *mut GEDevDesc,
) {
    enter(|| graphics_engine::text(x, y, &decode(text, encoding), xc, yc, rotation, &*gc, dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GESymbol(
    x: f64,
    y: f64,
    pch: c_int,
    size: f64,
    gc: *mut GEContext,
    dd: *mut GEDevDesc,
) {
    enter(|| graphics_engine::symbol(x, y, pch, size, &mut *gc, dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEStroke(path: Sexp, gc: *mut GEContext, dd: *mut GEDevDesc) {
    enter(|| graphics_engine::stroke(path, &*gc, dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEFill(path: Sexp, rule: c_int, gc: *mut GEContext, dd: *mut GEDevDesc) {
    enter(|| graphics_engine::fill(path, rule, &*gc, dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEFillStroke(
    path: Sexp,
    rule: c_int,
    gc: *mut GEContext,
    dd: *mut GEDevDesc,
) {
    enter(|| graphics_engine::fill_stroke(path, rule, &*gc, dd))
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn GEGlyph(
    count: c_int,
    glyphs: *mut c_int,
    x: *mut f64,
    y: *mut f64,
    font: Sexp,
    size: f64,
    colour: c_int,
    rotation: f64,
    dd: *mut GEDevDesc,
) {
    enter(|| {
        graphics_engine::glyph(
            slice_mut(glyphs, count),
            slice_mut(x, count),
            slice_mut(y, count),
            font,
            size,
            colour,
            rotation,
            dd,
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEPretty(low: *mut f64, high: *mut f64, divisions: *mut c_int) {
    enter(|| graphics_engine::pretty_axis(&mut *low, &mut *high, &mut *divisions))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEMetricInfo(
    code: c_int,
    gc: *mut GEContext,
    ascent: *mut f64,
    descent: *mut f64,
    width: *mut f64,
    dd: *mut GEDevDesc,
) {
    enter(|| {
        let metrics = graphics_engine::metric_info(code, &*gc, dd)?;
        write_metrics(metrics, ascent, descent, width);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEStrWidth(
    text: *const c_char,
    encoding: c_int,
    gc: *mut GEContext,
    dd: *mut GEDevDesc,
) -> f64 {
    enter(|| graphics_engine::str_width(&decode(text, encoding), &*gc, dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEStrHeight(
    text: *const c_char,
    encoding: c_int,
    gc: *mut GEContext,
    dd: *mut GEDevDesc,
) -> f64 {
    enter(|| graphics_engine::str_height(&decode(text, encoding), &*gc, dd))
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn GEStrMetric(
    text: *const c_char,
    encoding: c_int,
    gc: *mut GEContext,
    ascent: *mut f64,
    descent: *mut f64,
    width: *mut f64,
    dd: *mut GEDevDesc,
) {
    enter(|| {
        let metrics = graphics_engine::str_metric(&decode(text, encoding), &*gc, dd)?;
        write_metrics(metrics, ascent, descent, width);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEExpressionWidth(
    expression: Sexp,
    gc: *mut GEContext,
    dd: *mut GEDevDesc,
) -> f64 {
    enter(|| graphics_engine::expression_width(expression, &*gc, dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEExpressionHeight(
    expression: Sexp,
    gc: *mut GEContext,
    dd: *mut GEDevDesc,
) -> f64 {
    enter(|| graphics_engine::expression_height(expression, &*gc, dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEExpressionMetric(
    expression: Sexp,
    gc: *mut GEContext,
    ascent: *mut f64,
    descent: *mut f64,
    width: *mut f64,
    dd: *mut GEDevDesc,
) {
    enter(|| {
        let metrics = graphics_engine::expression_metric(expression, &*gc, dd)?;
        write_metrics(metrics, ascent, descent, width);
        Ok(())
    })
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn GEMathText(
    x: f64,
    y: f64,
    expression: Sexp,
    xc: f64,
    yc: f64,
    rotation: f64,
    gc: *mut GEContext,
    dd: *mut GEDevDesc,
) {
    enter(|| graphics_engine::math_text(x, y, expression, xc, yc, rotation, &*gc, dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEdeviceDirty(dd: *mut GEDevDesc) -> crate::Rboolean {
    crate::Rboolean::from(graphics_engine::device_dirty(dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEdirtyDevice(dd: *mut GEDevDesc) {
    graphics_engine::dirty_device(dd)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEcheckState(dd: *mut GEDevDesc) -> crate::Rboolean {
    enter(|| Ok(crate::Rboolean::from(graphics_engine::check_state(dd)?)))
}

/// `GErecording(call, dd)`: whether an operation made by `call` goes on the display list.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn GErecording(call: Sexp, dd: *mut GEDevDesc) -> crate::Rboolean {
    crate::Rboolean::from(call != runtime().nil() && (*dd).record_graphics != 0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GErecordGraphicOperation(op: Sexp, args: Sexp, dd: *mut GEDevDesc) {
    graphics_engine::record_operation(op, args, dd)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEinitDisplayList(dd: *mut GEDevDesc) {
    enter(|| graphics_engine::init_display_list(dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEplayDisplayList(dd: *mut GEDevDesc) {
    enter(|| graphics_engine::play_display_list(dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEcopyDisplayList(from: c_int) {
    enter(|| {
        let from = usize::try_from(from).map_err(|_| error("invalid device"))?;
        graphics_engine::copy_display_list(from)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEcreateSnapshot(dd: *mut GEDevDesc) -> Sexp {
    enter(|| graphics_engine::create_snapshot(dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEplaySnapshot(snapshot: Sexp, dd: *mut GEDevDesc) {
    enter(|| graphics_engine::play_snapshot(snapshot, dd))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEonExit() {
    enter(|| graphics_engine::on_exit())
}

/// `GEcontourLines(x, nx, y, ny, z, levels, nl)`: the contour lines as `contourLines()` returns
/// them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn GEcontourLines(
    x: *mut f64,
    columns: c_int,
    y: *mut f64,
    rows: c_int,
    z: *mut f64,
    levels: *mut f64,
    level_count: c_int,
) -> Sexp {
    enter(|| {
        let cells = columns.max(0).saturating_mul(rows.max(0));
        graphics::contour_lines(
            slice(x, columns),
            slice(y, rows),
            slice(z, cells),
            slice(levels, level_count),
        )
    })
}

/// `GAxisPars(&min, &max, &n, log, axis)`: the first and last tick and the interval count of
/// an axis over `[min, max]`, which are decades on a log axis.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_GAxisPars(
    min: *mut f64,
    max: *mut f64,
    count: *mut c_int,
    log: crate::Rboolean,
    _axis: c_int,
) {
    let swap = *min > *max;
    let (low, high) = if swap { (*max, *min) } else { (*min, *max) };
    let (mut first, mut last, intervals) = if log != 0 {
        graphics::log_axis(
            10f64.powf(low.max(-307.0)),
            10f64.powf(high.min(308.0)),
            *count,
        )
    } else {
        graphics::linear_axis(low, high, *count)
    };
    if swap {
        std::mem::swap(&mut first, &mut last);
    }
    *min = first;
    *max = last;
    *count = intervals;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_CreateAtVector(
    axp: *const f64,
    usr: *const f64,
    intervals: c_int,
    log: crate::Rboolean,
) -> Sexp {
    enter(|| {
        let axp = [*axp, *axp.add(1), *axp.add(2)];
        // Only a log axis reads `usr`, so grid passes NULL for its linear axes.
        let usr = if usr.is_null() {
            [axp[0], axp[1]]
        } else {
            [*usr, *usr.add(1)]
        };
        values::real_vector(&graphics::at_vector(axp, usr, intervals, log != 0))
    })
}

const CE_LATIN1: c_int = 2;

unsafe fn text_of(text: *const c_char) -> String {
    String::from_utf8_lossy(c_bytes(text)).into_owned()
}

/// Decodes a string in the encoding (`cetype_t`) native code passes with it.
unsafe fn decode(text: *const c_char, encoding: c_int) -> String {
    let bytes = c_bytes(text);
    if encoding == CE_LATIN1 {
        bytes.iter().map(|&byte| char::from(byte)).collect()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn index_of(index: c_int) -> usize {
    usize::try_from(index).unwrap_or(0)
}

unsafe fn slice<'a, T>(data: *const T, count: c_int) -> &'a [T] {
    match usize::try_from(count) {
        Ok(count) if count > 0 && !data.is_null() => std::slice::from_raw_parts(data, count),
        _ => &[],
    }
}

unsafe fn slice_mut<'a, T>(data: *mut T, count: c_int) -> &'a mut [T] {
    match usize::try_from(count) {
        Ok(count) if count > 0 && !data.is_null() => std::slice::from_raw_parts_mut(data, count),
        _ => &mut [],
    }
}

unsafe fn write_metrics([a, d, w]: [f64; 3], ascent: *mut f64, descent: *mut f64, width: *mut f64) {
    *ascent = a;
    *descent = d;
    *width = w;
}

#[unsafe(no_mangle)]
pub extern "C" fn R_GE_getVersion() -> c_int {
    GE_VERSION
}

/// `R_GE_checkVersionOrDie(version)`: GNU R accepts only its own version, but ry's engine reads
/// every device description from version 14 on, so packages built against R 4.1 to 4.6 run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_checkVersionOrDie(version: c_int) {
    if !(graphics_engine::OLDEST_ENGINE_VERSION..=GE_VERSION).contains(&version) {
        raise(error(format!(
            "Graphics API version mismatch: the device expects version {version}, ry provides \
             versions {} to {GE_VERSION}",
            graphics_engine::OLDEST_ENGINE_VERSION
        )));
    }
}

// Patterns, clipping paths, masks and glyphs are plain lists and attributes built by grDevices
// (`patterns.R`, `clippath.R`, `mask.R`, `glyph.R`), so their component positions are part of the
// device API.
const LINEAR_GRADIENT: c_int = 1;
const RADIAL_GRADIENT: c_int = 2;
const TILING_PATTERN: c_int = 3;

const GLYPH_ROTATION: usize = 6;
const FONT_VARIATIONS: usize = 6;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_isPattern(value: Sexp) -> Rboolean {
    enter(|| Ok(inherits(value, "Pattern")? as Rboolean))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_patternType(pattern: Sexp) -> c_int {
    enter(|| pattern_type(pattern))
}

macro_rules! pattern_scalars {
    ($($name:ident: $wanted:expr, $index:expr => $read:ident -> $output:ty;)*) => {
        $(
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn $name(pattern: Sexp) -> $output {
                enter(|| $read(pattern_component(pattern, $wanted, $index)?, 0))
            }
        )*
    };
}

pattern_scalars! {
    R_GE_linearGradientX1: LINEAR_GRADIENT, 1 => real_at -> f64;
    R_GE_linearGradientY1: LINEAR_GRADIENT, 2 => real_at -> f64;
    R_GE_linearGradientX2: LINEAR_GRADIENT, 3 => real_at -> f64;
    R_GE_linearGradientY2: LINEAR_GRADIENT, 4 => real_at -> f64;
    R_GE_linearGradientExtend: LINEAR_GRADIENT, 7 => integer_at -> c_int;
    R_GE_radialGradientCX1: RADIAL_GRADIENT, 1 => real_at -> f64;
    R_GE_radialGradientCY1: RADIAL_GRADIENT, 2 => real_at -> f64;
    R_GE_radialGradientR1: RADIAL_GRADIENT, 3 => real_at -> f64;
    R_GE_radialGradientCX2: RADIAL_GRADIENT, 4 => real_at -> f64;
    R_GE_radialGradientCY2: RADIAL_GRADIENT, 5 => real_at -> f64;
    R_GE_radialGradientR2: RADIAL_GRADIENT, 6 => real_at -> f64;
    R_GE_radialGradientExtend: RADIAL_GRADIENT, 9 => integer_at -> c_int;
    R_GE_tilingPatternX: TILING_PATTERN, 2 => real_at -> f64;
    R_GE_tilingPatternY: TILING_PATTERN, 3 => real_at -> f64;
    R_GE_tilingPatternWidth: TILING_PATTERN, 4 => real_at -> f64;
    R_GE_tilingPatternHeight: TILING_PATTERN, 5 => real_at -> f64;
    R_GE_tilingPatternExtend: TILING_PATTERN, 6 => integer_at -> c_int;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_linearGradientNumStops(pattern: Sexp) -> c_int {
    enter(|| Ok(xlength(pattern_component(pattern, LINEAR_GRADIENT, 5)?)? as c_int))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_linearGradientStop(pattern: Sexp, index: c_int) -> f64 {
    enter(|| real_at(pattern_component(pattern, LINEAR_GRADIENT, 5)?, index))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_linearGradientColour(pattern: Sexp, index: c_int) -> RColor {
    enter(|| {
        let colours = pattern_component(pattern, LINEAR_GRADIENT, 6)?;
        color_at(colours, index, TRANSPARENT_WHITE)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_radialGradientNumStops(pattern: Sexp) -> c_int {
    enter(|| Ok(xlength(pattern_component(pattern, RADIAL_GRADIENT, 7)?)? as c_int))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_radialGradientStop(pattern: Sexp, index: c_int) -> f64 {
    enter(|| real_at(pattern_component(pattern, RADIAL_GRADIENT, 7)?, index))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_radialGradientColour(pattern: Sexp, index: c_int) -> RColor {
    enter(|| {
        let colours = pattern_component(pattern, RADIAL_GRADIENT, 8)?;
        color_at(colours, index, TRANSPARENT_WHITE)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_tilingPatternFunction(pattern: Sexp) -> Sexp {
    enter(|| pattern_component(pattern, TILING_PATTERN, 1))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_clipPathFillRule(path: Sexp) -> c_int {
    enter(|| {
        let rule = runtime().heap.install(b"rule")?;
        integer_at(get_attrib(path, rule)?, 0)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_maskType(mask: Sexp) -> c_int {
    enter(|| {
        let kind = runtime().heap.install(b"type")?;
        integer_at(get_attrib(mask, kind)?, 0)
    })
}

macro_rules! list_components {
    ($($name:ident($argument:ident): $index:expr;)*) => {
        $(
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn $name($argument: Sexp) -> Sexp {
                enter(|| component($argument, $index))
            }
        )*
    };
}

list_components! {
    R_GE_glyphInfoGlyphs(glyph_info): 0;
    R_GE_glyphInfoFonts(glyph_info): 1;
    R_GE_glyphID(glyphs): 0;
    R_GE_glyphX(glyphs): 1;
    R_GE_glyphY(glyphs): 2;
    R_GE_glyphFont(glyphs): 3;
    R_GE_glyphSize(glyphs): 4;
    R_GE_glyphColour(glyphs): 5;
    R_GE_glyphRotation(glyphs): GLYPH_ROTATION;
}

/// Glyph information from R before 4.6 has no rotation component, so a device asks first.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_hasGlyphRotation(glyphs: Sexp) -> Rboolean {
    enter(|| Ok((xlength(glyphs)? > GLYPH_ROTATION as isize) as Rboolean))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_glyphFontFile(font: Sexp) -> *const c_char {
    enter(|| char_at(component(font, 0)?, 0))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_glyphFontIndex(font: Sexp) -> c_int {
    enter(|| integer_at(component(font, 1)?, 0))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_glyphFontFamily(font: Sexp) -> *const c_char {
    enter(|| char_at(component(font, 2)?, 0))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_glyphFontWeight(font: Sexp) -> f64 {
    enter(|| real_at(component(font, 3)?, 0))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_glyphFontStyle(font: Sexp) -> c_int {
    enter(|| integer_at(component(font, 4)?, 0))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_glyphFontPSname(font: Sexp) -> *const c_char {
    enter(|| char_at(component(font, 5)?, 0))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_glyphFontNumVar(font: Sexp) -> c_int {
    enter(|| Ok(xlength(component(font, FONT_VARIATIONS)?)? as c_int))
}

/// grDevices always names the font variations, so the axis names exist for every value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_glyphFontVarAxis(font: Sexp, index: c_int) -> *const c_char {
    enter(|| {
        let variations = font_variations(font, index)?;
        char_at(get_attrib(variations, R_NamesSymbol.get())?, index)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_glyphFontVarValue(font: Sexp, index: c_int) -> f64 {
    enter(|| real_at(font_variations(font, index)?, index))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_glyphFontVarFormatted(font: Sexp, index: c_int) -> *const c_char {
    enter(|| {
        let variations = font_variations(font, index)?;
        let formatted = runtime().heap.install(b"formatted")?;
        char_at(get_attrib(variations, formatted)?, index)
    })
}

/// `R_GE_rasterScale(source, sw, sh, destination, dw, dh)`: nearest-neighbour scaling into a
/// preallocated `dw` by `dh` image.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_rasterScale(
    source: *mut c_uint,
    source_width: c_int,
    source_height: c_int,
    destination: *mut c_uint,
    destination_width: c_int,
    destination_height: c_int,
) {
    let source_pixels = raster(source, source_width, source_height);
    let destination_pixels = raster_mut(destination, destination_width, destination_height);
    let (source_width, source_height) = (source_width as i64, source_height as i64);
    let (width, height) = (destination_width as i64, destination_height as i64);
    for (row, line) in destination_pixels
        .chunks_exact_mut(width.max(1) as usize)
        .enumerate()
    {
        let y = row as i64 * source_height / height;
        for (column, slot) in line.iter_mut().enumerate() {
            let x = column as i64 * source_width / width;
            *slot = pixel_at(source_pixels, source_width, x, y);
        }
    }
}

/// `R_GE_rasterInterpolate(source, sw, sh, destination, dw, dh)`: bilinear scaling into a
/// preallocated `dw` by `dh` image. Each destination pixel is split into 16 by 16 sub-pixels, and
/// the four nearest source pixels are weighted by how many sub-pixels each covers, which is the
/// scheme of leptonica's `scaleColorLILow`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_rasterInterpolate(
    source: *mut c_uint,
    source_width: c_int,
    source_height: c_int,
    destination: *mut c_uint,
    destination_width: c_int,
    destination_height: c_int,
) {
    let source_pixels = raster(source, source_width, source_height);
    let destination_pixels = raster_mut(destination, destination_width, destination_height);
    let scale_x = 16.0 * source_width as f64 / destination_width as f64;
    let scale_y = 16.0 * source_height as f64 / destination_height as f64;
    let source_width = source_width as i64;
    let last_x = source_width - 2;
    let last_y = source_height as i64 - 2;
    let width = (destination_width as usize).max(1);
    for (row, line) in destination_pixels.chunks_exact_mut(width).enumerate() {
        let y_position = (scale_y * row as f64 - 8.0).max(0.0) as i64;
        let (y, y_fraction) = (y_position >> 4, y_position & 0x0f);
        for (column, slot) in line.iter_mut().enumerate() {
            let x_position = (scale_x * column as f64 - 8.0).max(0.0) as i64;
            let (x, x_fraction) = (x_position >> 4, x_position & 0x0f);
            let pixel = |x, y| pixel_at(source_pixels, source_width, x, y);
            let top_left = pixel(x, y);
            // At the right and bottom edges the missing neighbours repeat the edge pixels.
            let [top_right, bottom_left, bottom_right] = if x <= last_x && y <= last_y {
                [pixel(x + 1, y), pixel(x, y + 1), pixel(x + 1, y + 1)]
            } else if x <= last_x {
                let right = pixel(x + 1, y);
                [right, top_left, right]
            } else if y <= last_y {
                let below = pixel(x, y + 1);
                [top_left, below, below]
            } else {
                [top_left; 3]
            };
            let corners = [top_left, top_right, bottom_left, bottom_right];
            *slot = blend(corners, x_fraction as u32, y_fraction as u32, true);
        }
    }
}

/// `R_GE_rasterRotatedSize(w, h, angle, wnew, hnew)`: the size that holds the image rotated by
/// `angle` radians, never smaller than the original.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_rasterRotatedSize(
    width: c_int,
    height: c_int,
    angle: f64,
    new_width: *mut c_int,
    new_height: *mut c_int,
) {
    let (width_f, height_f) = (width as f64, height as f64);
    let diagonal = (width_f * width_f + height_f * height_f).sqrt();
    let theta = height_f.atan2(width_f);
    let rotated_x = (diagonal * (theta + angle).cos())
        .abs()
        .max((diagonal * (theta - angle).cos()).abs());
    let rotated_y = (diagonal * (theta + angle).sin())
        .abs()
        .max((diagonal * (angle - theta).sin()).abs());
    *new_width = width.max((rotated_x + 0.5) as c_int);
    *new_height = height.max((rotated_y + 0.5) as c_int);
}

/// `R_GE_rasterRotatedOffset(w, h, angle, botleft, xoff, yoff)`: where the bottom-left (or, when
/// `botleft` is zero, the top-left) corner moves when the image turns by `angle` radians about
/// its centre.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_rasterRotatedOffset(
    width: c_int,
    height: c_int,
    angle: f64,
    bottom_left: c_int,
    x_offset: *mut f64,
    y_offset: *mut f64,
) {
    let (width_f, height_f) = (width as f64, height as f64);
    let half_diagonal = 0.5 * (width_f * width_f + height_f * height_f).sqrt();
    let corner = height_f.atan2(width_f);
    let theta = if bottom_left != 0 {
        std::f64::consts::PI + corner
    } else {
        -std::f64::consts::PI - corner
    };
    // GNU R halves the integer sizes with integer division, which matters for odd sizes.
    let half_width = (width / 2) as f64;
    let half_height = (height / 2) as f64;
    *x_offset = half_diagonal * (theta + angle).cos() + half_width;
    let rise = half_diagonal * (theta + angle).sin();
    *y_offset = if bottom_left != 0 {
        rise + half_height
    } else {
        rise - half_height
    };
}

/// `R_GE_rasterResizeForRotation(source, w, h, destination, wnew, hnew, gc)`: centres the image
/// in a preallocated `wnew` by `hnew` image filled with `gc->fill`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_rasterResizeForRotation(
    source: *mut c_uint,
    width: c_int,
    height: c_int,
    destination: *mut c_uint,
    new_width: c_int,
    new_height: c_int,
    context: *const GEContext,
) {
    let source_pixels = raster(source, width, height);
    let destination_pixels = raster_mut(destination, new_width, new_height);
    destination_pixels.fill((*context).fill as RColor);
    let (width, height) = (width as i64, height as i64);
    let (new_width, new_height) = (new_width as i64, new_height as i64);
    let x_shift = (new_width - width) / 2;
    let y_shift = (new_height - height) / 2;
    for row in 0..height {
        let target_row = row + y_shift;
        if !(0..new_height).contains(&target_row) {
            continue;
        }
        for column in 0..width {
            let target_column = column + x_shift;
            if !(0..new_width).contains(&target_column) {
                continue;
            }
            let target = (target_row * new_width + target_column) as usize;
            if let Some(slot) = destination_pixels.get_mut(target) {
                *slot = pixel_at(source_pixels, width, column, row);
            }
        }
    }
}

/// `R_GE_rasterRotate(source, w, h, angle, destination, gc, smoothAlpha)`: rotates the image
/// anticlockwise by `angle` radians about its centre into a preallocated image of the same size,
/// following leptonica's `rotateAMColorLow`. Pixels that come from outside the source take
/// `gc->fill`. With `smoothAlpha` the alpha channel is interpolated like the colours; without it
/// each pixel takes the largest alpha of its four neighbours, so fully transparent and fully
/// opaque regions keep their alpha exactly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GE_rasterRotate(
    source: *mut c_uint,
    width: c_int,
    height: c_int,
    angle: f64,
    destination: *mut c_uint,
    context: *const GEContext,
    smooth_alpha: Rboolean,
) {
    let source_pixels = raster(source, width, height);
    let destination_pixels = raster_mut(destination, width, height);
    let fill = (*context).fill as RColor;
    // leptonica measures angles clockwise.
    let angle = -angle;
    let sine = 16.0 * angle.sin();
    let cosine = 16.0 * angle.cos();
    let width = width as i64;
    let center_x = width / 2;
    let center_y = height as i64 / 2;
    let last_x = width - 2;
    let last_y = height as i64 - 2;
    for (row, line) in destination_pixels
        .chunks_exact_mut(width.max(1) as usize)
        .enumerate()
    {
        let y_distance = (center_y - row as i64) as f64;
        for (column, slot) in line.iter_mut().enumerate() {
            let x_distance = (center_x - column as i64) as f64;
            let x_position = (-x_distance * cosine - y_distance * sine) as i64;
            let y_position = (-y_distance * cosine + x_distance * sine) as i64;
            let x = center_x + (x_position >> 4);
            let y = center_y + (y_position >> 4);
            if x < 0 || y < 0 || x > last_x || y > last_y {
                *slot = fill;
                continue;
            }
            let pixel = |x, y| pixel_at(source_pixels, width, x, y);
            let corners = [
                pixel(x, y),
                pixel(x + 1, y),
                pixel(x, y + 1),
                pixel(x + 1, y + 1),
            ];
            *slot = blend(
                corners,
                (x_position & 0x0f) as u32,
                (y_position & 0x0f) as u32,
                smooth_alpha != 0,
            );
        }
    }
}

unsafe fn pattern_type(pattern: Sexp) -> ApiResult<c_int> {
    integer_at(component(pattern, 0)?, 0)
}

unsafe fn pattern_component(pattern: Sexp, wanted: c_int, index: usize) -> ApiResult<Sexp> {
    if pattern_type(pattern)? != wanted {
        let kind = match wanted {
            LINEAR_GRADIENT => "a linear gradient",
            RADIAL_GRADIENT => "a radial gradient",
            _ => "a tiling pattern",
        };
        return Err(error(format!("pattern is not {kind}")));
    }
    component(pattern, index)
}

/// The font variations list, after checking that `index` names one of them.
unsafe fn font_variations(font: Sexp, index: c_int) -> ApiResult<Sexp> {
    let variations = component(font, FONT_VARIATIONS)?;
    if index < 0 || index as isize >= xlength(variations)? {
        return Err(error("Index out of bounds"));
    }
    Ok(variations)
}

unsafe fn component(list: Sexp, index: usize) -> ApiResult<Sexp> {
    if list.kind() != SexpType::List {
        return Err(error(format!(
            "VECTOR_ELT() can only be applied to a 'list', not a '{}'",
            list.kind().name()
        )));
    }
    sexp_elt(list, checked(list, index as i64, "VECTOR_ELT")?)
}

/// GNU R reads these fields without a check, so a malformed object is undefined behaviour
/// there. Here it raises the error the matching accessor would.
unsafe fn integer_at(value: Sexp, index: c_int) -> ApiResult<c_int> {
    if !matches!(value.kind(), SexpType::Integer | SexpType::Logical) {
        return Err(error(format!(
            "INTEGER() can only be applied to a 'integer', not a '{}'",
            value.kind().name()
        )));
    }
    int_elt(value, checked(value, index as i64, "INTEGER")?)
}

unsafe fn real_at(value: Sexp, index: c_int) -> ApiResult<f64> {
    if value.kind() != SexpType::Real {
        return Err(error(format!(
            "REAL() can only be applied to a 'numeric', not a '{}'",
            value.kind().name()
        )));
    }
    real_elt(value, checked(value, index as i64, "REAL")?)
}

unsafe fn char_at(strings: Sexp, index: c_int) -> ApiResult<*const c_char> {
    if strings.kind() != SexpType::String {
        return Err(error(format!(
            "STRING_ELT() can only be applied to a 'character vector', not a '{}'",
            strings.kind().name()
        )));
    }
    let element = sexp_elt(strings, checked(strings, index as i64, "STRING_ELT")?)?;
    Ok(element.data() as *const c_char)
}

unsafe fn checked(value: Sexp, index: i64, accessor: &str) -> ApiResult<usize> {
    let length = xlength(value)?;
    if index < 0 || index >= length as i64 {
        return Err(error(format!(
            "attempt access index {index}/{length} in {accessor}"
        )));
    }
    Ok(index as usize)
}

/// A device's raster buffer, empty when the size is not positive.
unsafe fn raster<'a>(pixels: *const c_uint, width: c_int, height: c_int) -> &'a [RColor] {
    match pixel_count(width, height) {
        0 => &[],
        count if !pixels.is_null() => std::slice::from_raw_parts(pixels, count),
        _ => &[],
    }
}

unsafe fn raster_mut<'a>(pixels: *mut c_uint, width: c_int, height: c_int) -> &'a mut [RColor] {
    match pixel_count(width, height) {
        0 => &mut [],
        count if !pixels.is_null() => std::slice::from_raw_parts_mut(pixels, count),
        _ => &mut [],
    }
}

fn pixel_count(width: c_int, height: c_int) -> usize {
    if width <= 0 || height <= 0 {
        0
    } else {
        width as usize * height as usize
    }
}

/// The pixel at column `x` and row `y`, or transparent black outside the image.
fn pixel_at(pixels: &[RColor], width: i64, x: i64, y: i64) -> RColor {
    if x < 0 || y < 0 || x >= width {
        return 0;
    }
    usize::try_from(y * width + x)
        .ok()
        .and_then(|offset| pixels.get(offset))
        .copied()
        .unwrap_or(0)
}

/// Mixes the top-left, top-right, bottom-left and bottom-right pixels by the area a 16 by 16
/// sub-pixel square at offset (`x_fraction`, `y_fraction`) covers in each. Without
/// `smooth_alpha` the alpha channel is the corners' largest.
fn blend(corners: [RColor; 4], x_fraction: u32, y_fraction: u32, smooth_alpha: bool) -> RColor {
    let weights = [
        (16 - x_fraction) * (16 - y_fraction),
        x_fraction * (16 - y_fraction),
        (16 - x_fraction) * y_fraction,
        x_fraction * y_fraction,
    ];
    let channel = |shift: u32| {
        let total: u32 = corners
            .iter()
            .zip(weights)
            .map(|(corner, weight)| weight * ((corner >> shift) & 0xFF))
            .sum();
        ((total + 128) >> 8) & 0xFF
    };
    let alpha = if smooth_alpha {
        channel(24)
    } else {
        corners.iter().map(|corner| corner >> 24).max().unwrap_or(0)
    };
    channel(0) | channel(8) << 8 | channel(16) << 16 | alpha << 24
}

/// ry's runtime always runs in a UTF-8 locale.
#[unsafe(no_mangle)]
pub static mbcslocale: crate::Global<crate::Rboolean> = crate::Global::new(1);

/// `Rf_ucstoutf8(s, c)`: writes the UTF-8 encoding of code point `c` into `s` (at least seven
/// bytes) with a terminating NUL, and returns the number of bytes before the NUL. A surrogate or
/// a value past U+10FFFF is an error, as in GNU R.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_ucstoutf8(buffer: *mut c_char, code: c_uint) -> usize {
    enter(|| {
        let character =
            char::from_u32(code).ok_or_else(|| error(format!("invalid Unicode point {code}")))?;
        let mut bytes = [0u8; 4];
        let encoded = character.encode_utf8(&mut bytes).as_bytes();
        std::ptr::copy_nonoverlapping(encoded.as_ptr(), buffer as *mut u8, encoded.len());
        *buffer.add(encoded.len()) = 0;
        Ok(encoded.len())
    })
}

/// `R_GetX11Image(d, ximage, width, height)`: copies the image of X11 device `d`. ry has no X11
/// device, so there is never an image to return.
#[unsafe(no_mangle)]
pub extern "C" fn R_GetX11Image(
    _device: c_int,
    _image: *mut std::ffi::c_void,
    _width: *mut c_int,
    _height: *mut c_int,
) -> crate::Rboolean {
    0
}

/// Removes the private-use code points some fonts map symbols to. Without a symbol font there
/// is nothing to remap, so the text is copied.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_utf8Toutf8NoPUA(input: *const c_char) -> *const c_char {
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
                let alpha = if digits.len() == 4 {
                    digits[3] * 17
                } else {
                    255
                };
                (digits[0] * 17)
                    | ((digits[1] * 17) << 8)
                    | ((digits[2] * 17) << 16)
                    | (alpha << 24)
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
            palette_color(number as i64)
                .ok_or_else(|| error("numerical color values must be >= 0, found -1"))
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

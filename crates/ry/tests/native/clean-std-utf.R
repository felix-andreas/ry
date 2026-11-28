# The UTF-8, UTF-16 and UTF-32 conversions, walks and appends.
units <- function(s: Str) -> I64 {
  List.len(Str.to_utf16(s)) * 100 + List.len(Str.to_utf32(s))
}
round_trip <- function(s: Str) -> Str {
  a <- Str.from_utf16_lossy(Str.to_utf16(s))
  b <- Str.from_utf32_lossy(Str.to_utf32(a))
  match (Str.from_utf8(Str.to_utf8(b))) {
    :Ok(text) => text
    :Err(_) => "bad"
  }
}
strict8 <- function(units: List<I64>) -> Str {
  match (Str.from_utf8(units)) {
    :Ok(text) => text
    :Err(:BadUtf8) => "bad8"
  }
}
strict16 <- function(units: List<I64>) -> Str {
  match (Str.from_utf16(units)) {
    :Ok(text) => text
    :Err(:BadUtf16) => "bad16"
  }
}
strict32 <- function(units: List<I64>) -> Str {
  match (Str.from_utf32(units)) {
    :Ok(text) => text
    :Err(:BadUtf32) => "bad32"
  }
}
lossy32 <- function(n: I64) -> Str Str.from_utf32_lossy([104, n, 0, 105])
checksum <- function(s: Str) -> I64 {
  Str.walk_utf8(s, 0, function(total: I64, byte: I64) -> I64 total * 31 %% 1000003 + byte)
}
weighted <- function(s: Str) -> I64 {
  Str.walk_utf8_with_index(s, 0, function(total: I64, byte: I64, index: I64) -> I64 total + byte * index)
}
appended <- function(xs: List<I64>, s: Str) -> List<I64> List.concat_utf8(xs, s)
from_empty <- function(s: Str) -> I64 List.len(List.concat_utf8([], s))
print(units("hé😀"))
print(round_trip("hé😀!"))
print(c(strict8([104, 105]), strict16([104, 105]), strict32([104, 105])))
print(c(strict8([255, 104]), strict16([55357, 104]), strict32([55357, 104])))
print(c(strict8([104, 0]), strict16([104, 0]), strict32([104, 0])))
print(c(strict16([55357, 56832]), strict32([-1]), strict32([128512])))
print(lossy32(-1))
print(lossy32(1114112))
print(checksum("hé😀"))
print(weighted("abc"))
print(List.to_r(appended([1, 2], "hé")))
print(from_empty("hé😀"))

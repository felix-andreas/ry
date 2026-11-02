# Standard library benchmark

Produced on 2026-11-02 by a full `just bench-stdlib` run at n = 1e6 on a profiling build. The run
predates two changes: the R inputs are now materialized doubles, so the 0.000 s R times for `List.sum`,
`List.range` and `Dict.from_list` (GNU R's deferred sequences) are stale, and `List.min` and `List.max`
now read a double Array's storage directly. `backlog.md` lists the rerun.
Each time is the best of up to three runs in seconds; the ratio is ry / R, so above 1 means ry is slower.
`check` compares a checksum of both results (`differ` means they disagree, `n/a` that one side has no
result). Rerun with `just bench-stdlib [filter] [n]`.

| function | ry (s) | R equivalent | R (s) | ratio | check | notes |
|---|---:|---|---:|---:|---|---|
| List.range | 0.0480 | `seq_len` | 0.0000 | inf | ok |  |
| List.sum | 0.0110 | `sum` | 0.0000 | inf | ok |  |
| Dict.from_list | 0.3810 | `setNames` | 0.0000 | inf | ok |  |
| List.sort_with | 12.0360 | `[order]` | 0.0640 | 188.06 | ok |  |
| Dict.update | 3.5970 | `tabulate match` | 0.0270 | 133.22 | ok |  |
| Str.starts_with | 0.4370 | `startsWith` | 0.0160 | 27.31 | ok |  |
| Str.ends_with | 0.3980 | `endsWith` | 0.0160 | 24.88 | ok |  |
| Dict.get | 1.8650 | `named vector [keys]` | 0.1190 | 15.67 | ok |  |
| Dict.contains | 1.0960 | `%in% names` | 0.0840 | 13.05 | ok |  |
| Str.to_utf32 | 1.7360 | `stringi::stri_enc_toutf32` | 0.1350 | 12.86 | ok | ry: array: `List.map()` received an Array, so its result must be one: an Array item must be a single logical, integer, double or character value or a ry value, not a list |
| Str.contains | 0.4290 | `grepl fixed` | 0.0340 | 12.62 | ok |  |
| List.join | 0.0130 | `do.call c` | 0.0020 | 6.50 | ok |  |
| Str.count_utf8_bytes | 0.0680 | `nchar bytes` | 0.0110 | 6.18 | ok |  |
| List.concat_utf8 | 0.1210 | `c charToRaw` | 0.0200 | 6.05 | ok |  |
| Str.with_prefix | 1.2230 | `stringi::stri_c` | 0.2310 | 5.29 | ok |  |
| Str.is_empty | 0.0490 | `!nzchar` | 0.0100 | 4.90 | ok |  |
| Str.trim_end | 0.4980 | `stringi::stri_trim_right` | 0.1130 | 4.41 | ok |  |
| Str.drop_prefix | 1.0480 | `startsWith substring` | 0.2380 | 4.40 | ok |  |
| Str.replace_first | 1.1810 | `stringr::str_replace` | 0.2990 | 3.95 | ok |  |
| Str.replace_each | 1.2380 | `stringi::stri_replace_all_fixed` | 0.3400 | 3.64 | ok |  |
| Str.repeat | 1.0390 | `stringr::str_dup` | 0.2890 | 3.60 | ok |  |
| Str.replace_last | 1.0600 | `stringi::stri_replace_last_fixed` | 0.3030 | 3.50 | ok |  |
| List.for_each | 0.6290 | `for loop` | 0.1820 | 3.46 | ok |  |
| Str.walk_utf8 | 0.4000 | `for loop charToRaw` | 0.1260 | 3.17 | ok |  |
| Str.from_utf32 | 1.1060 | `stringi::stri_enc_fromutf32` | 0.3560 | 3.11 | ok |  |
| Str.trim | 0.4970 | `stringr::str_trim` | 0.1640 | 3.03 | ok |  |
| List.walk | 0.4130 | `for loop` | 0.1380 | 2.99 | ok |  |
| Str.from_utf8_lossy | 0.9780 | `stringi::stri_enc_fromutf32 (ASCII)` | 0.3380 | 2.89 | ok |  |
| Str.drop_suffix | 1.1560 | `endsWith substr` | 0.4530 | 2.55 | ok |  |
| Str.trim_start | 0.4830 | `stringr::str_trim` | 0.2010 | 2.40 | ok |  |
| List.from_vector | 0.0620 | `as.list` | 0.0260 | 2.38 | ok |  |
| Str.from_utf8 | 0.8340 | `stringi::stri_enc_fromutf32 (ASCII)` | 0.3500 | 2.38 | ok |  |
| List.walk_until | 0.3500 | `for loop` | 0.1470 | 2.38 | ok |  |
| List.concat | 0.3440 | `c` | 0.1470 | 2.34 | ok |  |
| List.ends_with | 0.1280 | `all ==` | 0.0560 | 2.29 | ok |  |
| List.max | 0.0360 | `max` | 0.0160 | 2.25 | ok |  |
| Dict.remove_all | 0.1960 | `named vector !%in%` | 0.0890 | 2.20 | ok |  |
| Str.from_utf32_lossy | 0.9120 | `stringi::stri_enc_fromutf32` | 0.4270 | 2.14 | ok |  |
| List.starts_with | 0.1510 | `all ==` | 0.0740 | 2.04 | ok |  |
| List.prepend | 0.1520 | `c` | 0.0750 | 2.03 | ok |  |
| List.drop_first | 0.0580 | `tail -k` | 0.0290 | 2.00 | ok |  |
| List.drop_last | 0.0560 | `[seq_len]` | 0.0280 | 2.00 | ok |  |
| List.sort_asc | 0.1230 | `sort radix` | 0.0640 | 1.92 | ok |  |
| List.take_first | 0.0530 | `[seq_len]` | 0.0280 | 1.89 | ok |  |
| List.walk_with_index_until | 0.4800 | `for loop` | 0.2580 | 1.86 | ok |  |
| List.min | 0.0310 | `min` | 0.0170 | 1.82 | ok |  |
| Str.concat | 0.5770 | `stringr::str_c` | 0.3340 | 1.73 | ok |  |
| List.reverse | 0.1490 | `rev` | 0.0870 | 1.71 | ok |  |
| List.len | 0.3280 | `length` | 0.1940 | 1.69 | ok |  |
| Str.split_first | 1.8470 | `regexpr substr` | 1.1970 | 1.54 | ok |  |
| Str.walk_utf8_with_index | 0.5030 | `for loop charToRaw` | 0.3270 | 1.54 | ok |  |
| List.walk_with_index | 0.5330 | `for loop` | 0.3510 | 1.52 | ok |  |
| Str.with_ascii_lowercased | 0.5190 | `stringr::str_to_lower` | 0.3440 | 1.51 | ok |  |
| List.repeat | 0.0060 | `rep` | 0.0040 | 1.50 | ok |  |
| Str.to_utf8 | 1.7020 | `lapply utf8ToInt (ASCII)` | 1.1350 | 1.50 | ok | ry: array: `List.map()` received an Array, so its result must be one: an Array item must be a single logical, integer, double or character value or a ry value, not a list |
| List.drop_at | 0.1560 | `[-i]` | 0.1050 | 1.49 | ok |  |
| List.walk_from | 0.1990 | `for loop` | 0.1410 | 1.41 | ok |  |
| Str.split_on | 0.2540 | `stringr::str_split_1` | 0.1800 | 1.41 | ok |  |
| List.prepend_if_ok | 0.1350 | `c` | 0.0960 | 1.41 | ok |  |
| List.split_last | 0.1790 | `max which` | 0.1360 | 1.32 | ok |  |
| Str.with_ascii_uppercased | 0.4300 | `stringr::str_to_upper` | 0.3320 | 1.30 | ok |  |
| Str.split_last | 1.3670 | `stringi::stri_locate_last_fixed` | 1.0770 | 1.27 | ok |  |
| List.walk_backwards | 0.3700 | `for loop` | 0.2940 | 1.26 | ok |  |
| List.sort_desc | 0.1230 | `sort radix decreasing` | 0.1030 | 1.19 | ok |  |
| List.walk_from_until | 0.1800 | `for loop` | 0.1510 | 1.19 | ok |  |
| List.split_first | 0.1820 | `match` | 0.1530 | 1.19 | ok |  |
| Dict.walk_until | 0.4160 | `for loop named vector` | 0.3560 | 1.17 | ok |  |
| List.first | 1.2290 | `[[1]]` | 1.1030 | 1.11 | ok |  |
| List.split_at | 0.1450 | `list(head, tail)` | 0.1330 | 1.09 | ok |  |
| List.walk_backwards_until | 0.3400 | `for loop` | 0.3140 | 1.08 | ok |  |
| List.intersperse | 0.0270 | `rbind` | 0.0250 | 1.08 | ok |  |
| List.take_last | 0.0530 | `tail` | 0.0500 | 1.06 | ok |  |
| List.get | 1.2370 | `[[` | 1.1690 | 1.06 | ok |  |
| List.to_vector | 0.0240 | `unlist` | 0.0230 | 1.04 | ok |  |
| Dict.clear | 0.0000 | `new.env` | 0.0000 | 1.00 | ok |  |
| List.sublist | 0.0540 | `[a:b]` | 0.0560 | 0.96 | ok |  |
| Dict.to_list | 0.1420 | `as.list + names` | 0.1500 | 0.95 | ok |  |
| Str.join_with | 0.0670 | `stringi::stri_flatten` | 0.0710 | 0.94 | ok |  |
| List.product | 0.0140 | `prod` | 0.0150 | 0.93 | ok |  |
| List.is_empty | 0.3010 | `length == 0` | 0.3370 | 0.89 | ok |  |
| Dict.keys | 0.0520 | `names` | 0.0620 | 0.84 | ok |  |
| List.find_first | 0.1750 | `Find` | 0.2090 | 0.84 | ok |  |
| List.any | 0.3410 | `Position` | 0.4110 | 0.83 | ok |  |
| List.single | 0.9550 | `list` | 1.1730 | 0.81 | ok |  |
| List.map_with_index | 0.5280 | `vapply` | 0.6490 | 0.81 | ok |  |
| Dict.single | 1.7100 | `named vector` | 2.1150 | 0.81 | ok |  |
| List.last | 1.0320 | `[[length]]` | 1.3200 | 0.78 | ok |  |
| List.update | 0.7950 | `[<- f([])` | 1.0380 | 0.77 | ok |  |
| List.find_last_index | 0.1630 | `Position right` | 0.2180 | 0.75 | ok |  |
| Str.to_utf16 | 1.7290 | `lapply utf8ToInt (BMP)` | 2.3150 | 0.75 | ok | ry: array: `List.map()` received an Array, so its result must be one: an Array item must be a single logical, integer, double or character value or a ry value, not a list |
| List.map | 0.3330 | `vapply` | 0.4790 | 0.70 | ok |  |
| List.find_first_index | 0.1550 | `Position` | 0.2310 | 0.67 | ok |  |
| Str.from_utf16 | 1.1010 | `vapply intToUtf8 (BMP)` | 1.6810 | 0.65 | ok |  |
| List.find_last | 0.1530 | `Find right` | 0.2370 | 0.65 | ok |  |
| Str.from_utf16_lossy | 1.1010 | `vapply intToUtf8 (BMP)` | 1.7230 | 0.64 | ok |  |
| List.for_each_try | 1.1870 | `for loop` | 1.8700 | 0.63 | ok |  |
| Str.to_integer | 0.1390 | `strtoi` | 0.2280 | 0.61 | ok |  |
| Result.map_err | 1.2930 | `lapply` | 2.2100 | 0.59 | ok |  |
| Result.on_err | 1.2460 | `lapply` | 2.1590 | 0.58 | ok |  |
| List.all | 0.2800 | `all vapply` | 0.4880 | 0.57 | ok |  |
| List.drop_if | 0.3570 | `[!vapply]` | 0.6250 | 0.57 | ok |  |
| Result.map | 1.1520 | `lapply` | 2.1260 | 0.54 | ok |  |
| List.keep_if | 0.3430 | `[vapply]` | 0.6970 | 0.49 | ok |  |
| List.count_if | 0.3180 | `sum vapply` | 0.6500 | 0.49 | ok |  |
| List.with_capacity | 0.2450 | `preallocated [[<-` | 0.5170 | 0.47 | ok |  |
| List.map2 | 0.4220 | `purrr::map2_dbl` | 0.8960 | 0.47 | ok |  |
| Dict.map | 0.5200 | `eapply` | 1.1240 | 0.46 | ok |  |
| Result.map2 | 1.5340 | `purrr::map2` | 3.3580 | 0.46 | ok |  |
| Result.and_then | 1.3630 | `lapply` | 3.3580 | 0.41 | ok |  |
| Dict.keep_shared | 0.2000 | `named vector %in%` | 0.5280 | 0.38 | ok |  |
| List.join_map | 0.3930 | `unlist lapply` | 1.0530 | 0.37 | ok |  |
| Dict.drop_if | 0.5160 | `[!vapply]` | 1.3960 | 0.37 | ok |  |
| Dict.keep_if | 0.5250 | `[vapply]` | 1.4590 | 0.36 | ok |  |
| List.set | 0.2160 | `[<-` | 0.6030 | 0.36 | ok |  |
| List.reserve | 0.2130 | `preallocated [[<-` | 0.6280 | 0.34 | ok |  |
| Result.map_both | 1.3950 | `lapply` | 4.3060 | 0.32 | ok |  |
| Dict.walk | 0.5430 | `Reduce as.list` | 1.6880 | 0.32 | ok |  |
| Result.with_default | 0.5390 | `vapply` | 1.6840 | 0.32 | ok |  |
| Dict.join_map | 1.9830 | `for loop env` | 6.9820 | 0.28 | ok |  |
| List.map3 | 0.5150 | `purrr::pmap_dbl` | 1.8140 | 0.28 | ok |  |
| List.append | 0.2540 | `vector [length + 1] <-` | 0.9270 | 0.27 | ok |  |
| List.append_if_ok | 0.5710 | `[length + 1] <-` | 2.1250 | 0.27 | ok |  |
| List.map4 | 0.6560 | `purrr::pmap_dbl` | 2.4750 | 0.27 | ok |  |
| List.swap | 0.3660 | `[<- pair` | 1.3980 | 0.26 | ok |  |
| Dict.empty | 0.2930 | `new.env` | 1.2510 | 0.23 | ok |  |
| List.walk_try | 0.6790 | `for loop` | 2.9390 | 0.23 | ok |  |
| Dict.values | 0.0230 | `as.list` | 0.1350 | 0.17 | ok |  |
| List.split_on | 0.0300 | `split cumsum` | 0.1800 | 0.17 | ok |  |
| List.map_try | 0.5480 | `for loop` | 3.3560 | 0.16 | ok |  |
| List.keep_errs | 0.7600 | `unlist lapply` | 4.8700 | 0.16 | ok |  |
| Dict.reserve | 1.1770 | `new.env size` | 7.7760 | 0.15 | ok |  |
| List.chunks_of | 0.0280 | `split` | 0.1970 | 0.14 | ok |  |
| List.keep_oks | 0.6090 | `unlist lapply` | 4.6790 | 0.13 | ok |  |
| Str.caseless_ascii_equals | 0.0990 | `stringi::stri_trans_tolower ==` | 0.7630 | 0.13 | ok |  |
| List.split_on_list | 0.0270 | `split cumsum` | 0.2180 | 0.12 | ok |  |
| Dict.insert | 0.9250 | `assign` | 7.6620 | 0.12 | ok |  |
| Dict.with_capacity | 0.8520 | `new.env size` | 7.1390 | 0.12 | ok |  |
| Str.to_double | 0.1430 | `as.double` | 1.2530 | 0.11 | ok |  |
| List.keep_if_try | 0.6000 | `[vapply]` | 5.6580 | 0.11 | ok |  |
| Result.is_ok | 0.1260 | `purrr::map_lgl` | 1.2220 | 0.10 | ok |  |
| List.contains | 0.0050 | `any ==` | 0.0490 | 0.10 | ok |  |
| Result.is_err | 0.1290 | `vapply` | 1.5280 | 0.08 | ok |  |
| Dict.len | 0.4130 | `length` | 8.5580 | 0.05 | ok |  |
| Dict.is_empty | 0.3130 | `length == 0` | 9.2100 | 0.03 | ok |  |
| List.replace | timeout | `[[ then [[<-` | 0.9410 |  | n/a | ry: over 60s |
| List.release_excess_capacity | 0.0130 | `none` |  |  | n/a |  |
| Dict.remove | timeout | `rm list` | 1.6060 |  | n/a | ry: over 60s |
| Dict.insert_all | 0.0840 | `timeout` |  |  | n/a | R: over 60s |
| Dict.capacity | 0.3590 | `none` |  |  | n/a |  |
| Dict.release_excess_capacity | 0.1020 | `none` |  |  | n/a |  |

/*
 * SPDX-License-Identifier: LGPL-2.1-or-later
 *
 * Copyright (C) 2025-2026 Felix Andreas. This header is part of ry. It is free software: you
 * can redistribute it and/or modify it under the terms of the GNU Lesser General Public License
 * as published by the Free Software Foundation, either version 2.1 of the License, or (at your
 * option) any later version. See LICENSE at the root of ry's repository.
 */
/* The version for code built directly against these headers. An R home that `rcmd::assemble`
   creates replaces this file with one generated from the base package the interpreter runs, so
   that R.version and the headers agree. The revision is at least 89194 either way, because ry
   implements the API data.table guards on it (`R_SVN_REVISION < 89194` keeps its backport of
   R_mapAttrib). */
#ifndef R_VERSION_H
#define R_VERSION_H

#define R_Version(v, p, s) (((v) * 65536) + ((p) * 256) + (s))
#define R_VERSION R_Version(4, 6, 2)
#define R_NICK "ry"
#define R_MAJOR "4"
#define R_MINOR "6.2"
#define R_STATUS ""
#define R_YEAR "2026"
#define R_MONTH "10"
#define R_DAY "31"
#define R_SVN_REVISION 89194
#define R_FILEVERSION 4, 62, 89194, 0

#endif

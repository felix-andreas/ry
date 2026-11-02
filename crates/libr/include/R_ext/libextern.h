/*
 * SPDX-License-Identifier: LGPL-2.1-or-later
 *
 * Copyright (C) 2025-2026 Felix Andreas. This header is part of ry. It is free software: you
 * can redistribute it and/or modify it under the terms of the GNU Lesser General Public License
 * as published by the Free Software Foundation, either version 2.1 of the License, or (at your
 * option) any later version. See LICENSE at the root of ry's repository.
 */
#ifndef R_LIBEXTERN_H_
#define R_LIBEXTERN_H_

/*
 * On Windows a package reaches R.dll's variables through the import table, so their
 * declarations carry dllimport; R.dll's own sources define R_DLL_BUILD and see plain externs.
 */
#if defined(_WIN32) && !defined(R_DLL_BUILD)
#define LibImport __declspec(dllimport)
#else
#define LibImport
#endif

#ifdef _WIN32
#define LibExport __declspec(dllexport)
#else
#define LibExport
#endif

#define LibExtern LibImport extern

#ifdef __cplusplus
#define LibStatic static
#endif

#endif

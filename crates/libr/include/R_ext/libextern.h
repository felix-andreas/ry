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

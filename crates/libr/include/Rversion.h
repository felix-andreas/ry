/* ry's libR presents itself as R 4.6.2, so packages that guard on the version take the branches
   written for current R. data.table also guards on the revision (`R_SVN_REVISION < 89194` keeps
   its backport of R_mapAttrib), so the revision is at least that. */
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

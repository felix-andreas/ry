/* Prints the size of struct Rconn and the offset of every member, one per line. Compiled
   against ry's headers and against GNU R's, the two outputs must be identical. */
#include <stddef.h>
#include <stdio.h>
#include <R.h>
#include <Rinternals.h>
#include <R_ext/Connections.h>

#define MEMBER(name) printf("%s %zu %zu\n", #name, offsetof(struct Rconn, name), sizeof(((struct Rconn *) 0)->name))

int main(void)
{
    printf("version %d\n", R_CONNECTIONS_VERSION);
    printf("sizeof %zu\n", sizeof(struct Rconn));
    MEMBER(class);
    MEMBER(description);
    MEMBER(enc);
    MEMBER(mode);
    MEMBER(text);
    MEMBER(isopen);
    MEMBER(incomplete);
    MEMBER(canread);
    MEMBER(canwrite);
    MEMBER(canseek);
    MEMBER(blocking);
    MEMBER(isGzcon);
    MEMBER(open);
    MEMBER(close);
    MEMBER(destroy);
    MEMBER(vfprintf);
    MEMBER(fgetc);
    MEMBER(fgetc_internal);
    MEMBER(seek);
    MEMBER(truncate);
    MEMBER(fflush);
    MEMBER(read);
    MEMBER(write);
    MEMBER(nPushBack);
    MEMBER(posPushBack);
    MEMBER(PushBack);
    MEMBER(save);
    MEMBER(save2);
    MEMBER(encname);
    MEMBER(inconv);
    MEMBER(outconv);
    MEMBER(iconvbuff);
    MEMBER(oconvbuff);
    MEMBER(next);
    MEMBER(init_out);
    MEMBER(navail);
    MEMBER(inavail);
    MEMBER(EOF_signalled);
    MEMBER(UTF8out);
    MEMBER(id);
    MEMBER(ex_ptr);
    MEMBER(private);
    MEMBER(status);
    MEMBER(buff);
    MEMBER(buff_len);
    MEMBER(buff_stored_len);
    MEMBER(buff_pos);
    return 0;
}

#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
typedef uint64_t u64;
#define R 256
#define WD 4
static u64 A[R][WD],B[R][WD];
static void step_big(void){
  for(int y=0;y<R;y++)for(int w=0;w<WD;w++){
    u64 rows[3],L[3],Rr[3];
    rows[0]=y>0?A[y-1][w]:0; rows[1]=A[y][w]; rows[2]=y<R-1?A[y+1][w]:0;
    for(int k=0;k<3;k++){int yy=y-1+k;u64 left=0,right=0;if(yy>=0&&yy<R){left=w>0?A[yy][w-1]:0;right=w<WD-1?A[yy][w+1]:0;}
      L[k]=(rows[k]<<1)|(left>>63);Rr[k]=(rows[k]>>1)|(right<<63);}
    u64 n[8]={L[0],rows[0],Rr[0],L[1],Rr[1],L[2],rows[2],Rr[2]};
    u64 one=0,two=0,four=0;
    for(int i=0;i<8;i++){u64 c1=one&n[i];one^=n[i];u64 c2=two&c1;two^=c1;four|=c2;}
    B[y][w]=(one&two&~four)|(rows[1]&~one&two&~four);
  }
  memcpy(A,B,sizeof A);
}
static u64 hash(void){u64 h=1469598103934665603ULL;for(int y=0;y<R;y++)for(int w=0;w<WD;w++){h^=A[y][w];h*=1099511628211ULL;}return h;}
static long pop(void){long p=0;for(int y=0;y<R;y++)for(int w=0;w<WD;w++)p+=__builtin_popcountll(A[y][w]);return p;}
static int edge(void){for(int w=0;w<WD;w++)if(A[0][w]||A[R-1][w])return 1;for(int y=0;y<R;y++)if((A[y][0]&1)||(A[y][WD-1]>>63))return 1;return 0;}
static void setc(int x,int y){A[y][x>>6]|=1ULL<<(x&63);}
// lifespan until stable (period<=40 seen twice) ; returns gens, -1 if left the grid / >limit
static int life(int limit,long*finalpop){
  u64 hist[64];int n=0;
  for(int t=1;t<=limit;t++){
    step_big();
    if(edge())return -1;
    u64 h=hash();
    for(int p=1;p<=40&&p<=n;p++)if(hist[(n-p)&63]==h){ // period p candidate: confirm 2p
      if(n>=2*p&&hist[(n-2*p)&63]==hist[(n-p)&63]){*finalpop=pop();return t-2*p;}
    }
    hist[n&63]=h;n++;
  }
  return -1;
}

static char art[32][32];
int main(int argc,char**argv){
  int S=atoi(argv[1]); int mode=atoi(argv[2]);
  int half=(S+1)/2; int rowsfree=(mode==4)?half:S; int bits=rowsfree*half; long total=1L<<bits;
  int cx=100,cy=100; long found=0;
  for(long m=1;m<total;m++){
    memset(A,0,sizeof A); memset(art,'.',sizeof art);
    for(int j=0;j<rowsfree;j++)for(int i=0;i<half;i++){
      if(!((m>>(j*half+i))&1))continue;
      int xs[2]={i,S-1-i},ys[2]={j,mode==4?S-1-j:j};
      for(int a=0;a<2;a++)for(int b=0;b<2;b++){int x=xs[a],y=ys[b];setc(cx+x,cy+y);art[y][x]='#';}
    }
    long cells=pop(); if(cells<6||cells>30)continue;
    long fp=0;int g=life(6000,&fp);
    if(g>=400){found++;printf("gens=%d cells=%ld final=%ld S=%d mode=%d\n",g,cells,fp,S,mode);
      for(int y=0;y<S;y++){for(int x=0;x<S;x++)putchar(art[y][x]);putchar('\n');}fflush(stdout);}
  }
  fprintf(stderr,"S=%d mode=%d found=%ld\n",S,mode,found);
}

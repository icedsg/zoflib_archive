import os,random,time,zlib,lzma,zstandard as z
from cryptography.hazmat.primitives.ciphers.aead import AESGCM,ChaCha20Poly1305
random.seed(7)
def sample(root,cap=256<<20,maxf=8<<20):
    c=[];n=0
    for d,ds,fs in os.walk(root):
        ds[:]=[x for x in ds if x not in('.git','node_modules','.venv','venv','__pycache__','.secrets') and not x.startswith(('_','.'))]
        random.shuffle(ds)
        for f in fs:
            p=os.path.join(d,f)
            try:
                s=os.path.getsize(p)
                if 0<s<=maxf and not f.startswith('.env'): c.append((p,s))
            except OSError: pass
        if len(c)>40000: break
    random.shuffle(c);out=[]
    for p,s in c:
        if n+s>cap: continue
        try: out.append(open(p,'rb').read());n+=s
        except OSError: pass
        if n>cap*0.97: break
    return b''.join(out),len(out)
import sys
for root in sys.argv[1:]:
    name=root
    if not os.path.isdir(root): continue
    d,k=sample(root)
    M=len(d)/1e6; print(f'\n{name}: {M:.0f} MB, {k} files',flush=True)
    if M<5: continue
    def show(lbl,c,tc,td):print(f'{lbl:14} ratio {len(d)/c:5.2f}  comp {M/tc:6.0f} MB/s  decomp {M/td:6.0f} MB/s',flush=True)
    for l,th in((-5,0),(1,0),(1,8),(3,0),(3,8),(9,8),(12,8),(16,8)):
        cc=z.ZstdCompressor(level=l,threads=th) 
        t=time.perf_counter();b=cc.compress(d);tc=time.perf_counter()-t
        t=time.perf_counter();r=z.ZstdDecompressor().decompress(b);td=time.perf_counter()-t
        assert r==d; show(f'zstd {l} t{max(th,1)}',len(b),tc,td)
    s=d[:32<<20]
    t=time.perf_counter();b=lzma.compress(s,preset=6);tc=time.perf_counter()-t
    t=time.perf_counter();lzma.decompress(b);td=time.perf_counter()-t
    print(f'xz 6 (32MB)    ratio {len(s)/len(b):5.2f}  comp {len(s)/1e6/tc:6.0f}  decomp {len(s)/1e6/td:6.0f}')
buf=os.urandom(1<<20)
for n,c in(('AES-256-GCM',AESGCM(os.urandom(32))),('ChaCha20-Poly1305',ChaCha20Poly1305(os.urandom(32)))):
    t=time.perf_counter()
    for i in range(400): c.encrypt(i.to_bytes(12,'big'),buf,None)
    print(n,f'{400*1.048576/(time.perf_counter()-t):.0f} MB/s, one thread')

import json, glob

class Dec:
    def __init__(self, b): self.b=b; self.i=0
    def u(self, ai):
        if ai<24: return ai
        if ai==24: v=self.b[self.i]; self.i+=1; return v
        if ai==25: v=int.from_bytes(self.b[self.i:self.i+2],'big'); self.i+=2; return v
        if ai==26: v=int.from_bytes(self.b[self.i:self.i+4],'big'); self.i+=4; return v
        if ai==27: v=int.from_bytes(self.b[self.i:self.i+8],'big'); self.i+=8; return v
        if ai==31: return None
        raise ValueError("ai")
    def item(self):
        ib=self.b[self.i]; self.i+=1; mt=ib>>5; ai=ib&0x1f
        if mt==0: return self.u(ai)
        if mt==1: return -1-self.u(ai)
        if mt==2:
            n=self.u(ai)
            if n is None:
                out=b''
                while self.b[self.i]!=0xff: out+=self.item()
                self.i+=1; return out
            v=self.b[self.i:self.i+n]; self.i+=n; return v
        if mt==3:
            n=self.u(ai)
            if n is None:
                s=''
                while self.b[self.i]!=0xff: s+=self.item()
                self.i+=1; return s
            v=self.b[self.i:self.i+n].decode('utf8','replace'); self.i+=n; return v
        if mt==4:
            n=self.u(ai); out=[]
            if n is None:
                while self.b[self.i]!=0xff: out.append(self.item())
                self.i+=1; return out
            for _ in range(n): out.append(self.item())
            return out
        if mt==5:
            n=self.u(ai); out=[]   # list of (k,v) pairs, byte keys preserved
            if n is None:
                while self.b[self.i]!=0xff:
                    k=self.item(); v=self.item(); out.append((k,v))
                self.i+=1; return ('MAP',out)
            for _ in range(n):
                k=self.item(); v=self.item(); out.append((k,v))
            return ('MAP',out)
        if mt==6:
            self.u(ai); return self.item()
        if mt==7:
            if ai==20: return False
            if ai==21: return True
            if ai==22: return None
            if ai in (25,26,27): self.u(ai); return 0.0
            return None
        raise ValueError("mt")

def body_get(body, key):
    if not (isinstance(body,tuple) and body[0]=='MAP'): return None
    for k,v in body[1]:
        if k==key: return v
    return None

def hdr_type(blob):
    if not isinstance(blob,(bytes,bytearray)) or len(blob)<1: return 'other'
    hi=blob[0]&0xF0
    return 'key' if hi==0xE0 else ('script' if hi==0xF0 else 'other')

wdrl_hits=[]; vote_hits=[]; errs=0; n=0; multi_wdrl=0; multi_vote=0
for f in sorted(glob.glob('phase2-dumps-730val/*.json')):
    n+=1
    try:
        o=json.load(open(f))
        pm=o.get('protocol_major')
        tx=Dec(bytes.fromhex(o['tx_cbor'])).item()
        body=tx[0] if isinstance(tx,list) else tx
        w=body_get(body,5)
        if isinstance(w,tuple) and w[0]=='MAP':
            accts=[hdr_type(k) for k,_ in w[1]]
            if len(accts)>=2: multi_wdrl+=1
            types=set(accts)
            if 'key' in types and 'script' in types:
                wdrl_hits.append((f,pm,accts))
        v=body_get(body,19)  # voting_procedures
        if isinstance(v,tuple) and v[0]=='MAP':
            voters=[]
            for voter,_votes in v[1]:
                # voter = [type_int, hash]
                if isinstance(voter,list) and len(voter)>=1 and isinstance(voter[0],int):
                    voters.append(voter[0])
            if len(voters)>=2: multi_vote+=1
            # mixed cred type within same role: DRep key(2)/script(3); CC key(0)/script(1)
            s=set(voters)
            if ({0,1}&s and len({0,1}&s)==2) or ({2,3}&s and len({2,3}&s)==2):
                vote_hits.append((f,pm,voters))
    except Exception as e:
        errs+=1
print(f"scanned={n} errs={errs}")
print(f"txs with >=2 withdrawals: {multi_wdrl}; with >=2 voters: {multi_vote}")
print(f"MIXED key+script WITHDRAWAL hits: {len(wdrl_hits)}")
for f,pm,a in wdrl_hits[:10]: print("  ",f.split('/')[-1],"pm",pm,a)
print(f"MIXED-cred VOTER hits: {len(vote_hits)}")
for f,pm,a in vote_hits[:10]: print("  ",f.split('/')[-1],"pm",pm,a)

# subset: dumps that EXERCISE the changed code (>=1 withdrawal or >=1 vote)
import json, glob
sub=[]
for f in sorted(glob.glob('phase2-dumps-730val/*.json')):
    try:
        o=json.load(open(f)); tx=Dec(bytes.fromhex(o['tx_cbor'])).item()
        body=tx[0] if isinstance(tx,list) else tx
        w=body_get(body,5); v=body_get(body,19)
        nw = len(w[1]) if isinstance(w,tuple) and w[0]=='MAP' else 0
        nv = len(v[1]) if isinstance(v,tuple) and v[0]=='MAP' else 0
        if nw>=1 or nv>=1: sub.append((f,nw,nv))
    except Exception: pass
print("SUBSET exercising changed code (>=1 wdrl or >=1 vote):", len(sub))
for f,nw,nv in sub[:20]: print("  ",f.split('/')[-1],"wdrl",nw,"vote",nv)
open('/tmp/repro_subset.txt','w').write('\n'.join(f for f,_,_ in sub))

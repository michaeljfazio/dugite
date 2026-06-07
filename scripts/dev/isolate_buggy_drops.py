import re, subprocess, json, sys
exec(open('/tmp/isolate_full.py').read().split('rows=[]')[0])  # reuse bech32 fns
rows=[]
for l in open('epoch-dumps-engine/mainnet-rupd-drop/ep246_drops.txt'):
    m=re.search(r'cred=(\w{56})00000000 would_be=(\d+) stake=(\d+)',l)
    if m: rows.append((m.group(1),int(m.group(2)),int(m.group(3))))
a2cred={addr(h):(h,w,s) for h,w,s in rows}
addrs=list(a2cred)
hist={}
B=70
for i in range(0,len(addrs),B):
    body=json.dumps({"_stake_addresses":addrs[i:i+B]})
    o=subprocess.run(["bash","scripts/prod-readiness/lib/koios.sh","mainnet","account_reward_history",body],capture_output=True,text=True,timeout=120)
    try:
        for r in json.loads(o.stdout): hist.setdefault(r['stake_address'],[]).append((r.get('earned_epoch'),int(r.get('amount',0))))
    except: pass
# classify each dropped cred by its koios reward history span
import json as J
recs=[]
for a,(h,w,s) in a2cred.items():
    ee=sorted(e for e,_ in hist.get(a,[]))
    recs.append({'cred':h,'wb':w,'stake':s,'n_rows':len(ee),'min_ee':ee[0] if ee else None,'max_ee':ee[-1] if ee else None,'has245':245 in ee,'has246':246 in ee,'has247':247 in ee})
def grp(pred):
    g=[r for r in recs if pred(r)]; return len(g), sum(r['wb'] for r in g)
print("TARGET buggy would_be sum ~82,270,482 (82,215,213 reg + 55,269 dereg)")
print("no koios history at all:", grp(lambda r:r['n_rows']==0))
print("has any history:", grp(lambda r:r['n_rows']>0))
print("still active (max_ee>=246):", grp(lambda r:r['max_ee'] is not None and r['max_ee']>=246))
print("still active (max_ee>=247):", grp(lambda r:r['max_ee'] is not None and r['max_ee']>=247))
print("last reward exactly 244 (dereg-after-244):", grp(lambda r:r['max_ee']==244))
print("last reward exactly 245:", grp(lambda r:r['max_ee']==245))
print("has246 row:", grp(lambda r:r['has246']))
print("has247 row:", grp(lambda r:r['has247']))
# the still-active ones are the prime buggy suspects: list top by wb
act=[r for r in recs if r['max_ee'] is not None and r['max_ee']>=246]
act.sort(key=lambda r:-r['wb'])
print("\nstill-active dropped creds (max_ee>=246), top 15 by would_be:")
for r in act[:15]: print(f"  {r['cred'][:14]}.. wb={r['wb']:>12} stake={r['stake']:>15} ee[{r['min_ee']}..{r['max_ee']}] n={r['n_rows']} 245={r['has245']} 246={r['has246']}")
J.dump(recs, open('epoch-dumps-engine/mainnet-rupd-drop/ep246_isolation.json','w'))
print("\nsaved -> epoch-dumps-engine/mainnet-rupd-drop/ep246_isolation.json")

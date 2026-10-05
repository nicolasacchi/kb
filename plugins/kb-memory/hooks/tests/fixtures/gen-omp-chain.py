#!/usr/bin/env python3
"""Synthetic omp session JSONL with a genuine id/parentId chain.
usage: gen.py LINES OUT [seed]"""
import sys, json, random, string
n=int(sys.argv[1]); out=sys.argv[2]; random.seed(int(sys.argv[3]) if len(sys.argv)>3 else 7)
def words(k): return " ".join(random.choice(["alpha","beta","widget","parse","lance","index","tokio","chunk","fn","let"])+str(random.randint(0,99)) for _ in range(k))
def blob(b):
    s=""; 
    while len(s)<b: s+=words(40)+"\n"
    return s[:b]
cnt=0
def nid(): 
    global cnt; cnt+=1; return "e%07x"%cnt
recs=[]
ts=1756022400000
def iso(t): 
    import datetime; return datetime.datetime.fromtimestamp(t/1000,datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.")+"%03dZ"%(t%1000)
def add(rec,parent):
    global ts; ts+=1000
    rec["id"]=nid(); rec["parentId"]=parent; rec["timestamp"]=iso(ts); recs.append(rec); return rec["id"]
def msg(role,content,**kw):
    m={"role":role,"content":content,"timestamp":ts+1000}; m.update(kw); return {"type":"message","message":m}
recs.append({"type":"session","version":3,"id":"syn00000-0000-0000-0000-000000000001","timestamp":iso(ts),"cwd":"/tmp/syn","title":"synthetic"})
recs.append({"type":"title","title":"Synthetic session".ljust(240)})
head=add({"type":"model_change","model":"prov/model-x"},None)
cur=head; did_reset=False
while len(recs)<n-6:
    r=random.random()
    if r<0.06:  # abandoned branch of 1-4 records off a recent node
        p=cur
        for _ in range(random.randint(1,4)):
            p=add(msg("assistant",[{"type":"text","text":"ABANDONED "+words(30)}]),p)
        continue
    if r<0.065:
        cur=add({"type":"compaction","summary":blob(3000),"shortSummary":words(8),"firstKeptEntryId":cur,"tokensBefore":50000,"details":{"readFiles":["/tmp/a%d.py"%i for i in range(5)],"modifiedFiles":["/tmp/b.py"]}},cur); continue
    if r<0.07:
        cur=add({"type":"custom","customType":"kb.recall","data":{"v":1,"markers":[{"kb":"memory","id":"%012x"%random.getrandbits(48),"title":words(5)}],"prompt_ts":iso(ts)}},cur); continue
    if not did_reset and len(recs)>n//10:
        did_reset=True; cur=add({"type":"reset_boundary"},cur); continue
    if r<0.10:
        cur=add({"type":"thinking_level_change","level":"high"},cur); continue
    if r<0.16:
        cur=add(msg("user",[{"type":"text","text":words(random.randint(5,80))}]),cur); continue
    # assistant turn with tool call + start + result
    tc="fc_%d"%cnt
    content=[{"type":"thinking","thinking":blob(random.randint(300,3000)),"thinkingSignature":"sig"},{"type":"text","text":words(30)},
             {"type":"toolCall","id":tc,"name":random.choice(["bash","read","write","edit","grep"]),"arguments":{"command":words(6),"path":"/tmp/f%d.py"%random.randint(0,50)}}]
    cur=add(msg("assistant",content,model="prov/model-x",usage={"input":random.randint(100,9000),"output":random.randint(10,900),"cacheRead":random.randint(0,50000),"cacheWrite":0,"totalTokens":1,"cost":{"total":0}},stopReason="toolUse"),cur)
    cur=add({"type":"custom","customType":"tool_execution_start","data":{"toolCallId":tc,"toolName":"bash","intent":words(6)}},cur)
    cur=add(msg("toolResult",[{"type":"text","text":blob(random.choice([200,800,2000,4000,9000]))}],toolCallId=tc,toolName="bash",isError=random.random()<0.05),cur)
cur=add({"type":"custom","customType":"session_exit","data":{"kind":"signal","pendingToolCalls":[{"toolCallId":"x"}]}},cur)
with open(out,"w") as f:
    for r in recs: f.write(json.dumps(r)+"\n")

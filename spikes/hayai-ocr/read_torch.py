import json,random,time,sys,torch
from PIL import Image
from transformers import AutoModel, AutoProcessor, PreTrainedTokenizerFast
M="JustANormalTinkerer/hayai-ocr-v2.5-nova"
model=AutoModel.from_pretrained(M,trust_remote_code=True).eval()
tok=PreTrainedTokenizerFast.from_pretrained(M)
proc=AutoProcessor.from_pretrained("google/siglip2-base-patch16-naflex")
def read(img,patches=384,maxn=64):
    inp=proc(images=[img],max_num_patches=patches,return_tensors="pt")
    with torch.no_grad():
        v=model.vision_encoder(pixel_values=inp["pixel_values"],pixel_attention_mask=inp["pixel_attention_mask"],spatial_shapes=inp["spatial_shapes"]).last_hidden_state
        ids=[tok.bos_token_id or 1]; probs=[]
        eos=tok.eos_token_id or 2
        for _ in range(maxn):
            lg=model.decoder(v,inp["spatial_shapes"],torch.tensor([ids]))[0,-1]
            p=lg.softmax(-1); t=int(p.argmax())
            if t==eos: probs.append(float(p[t])); break
            ids.append(t); probs.append(float(p[t]))
    return tok.decode(ids[1:],skip_special_tokens=True),probs
if __name__=="__main__":
    rows=json.load(open("crops/index.json"))
    out=[]
    t0=time.time()
    for r in rows:
        img=Image.open("crops/"+r["name"]).convert("RGB")
        text,probs=read(img)
        r.update(text=text,minp=min(probs),meanp=sum(probs)/len(probs),n=len(probs))
        print(f'{r["name"]:22s} {r["kind"]:4s} {r["reason"].split(".")[-1][:22]:22s} min={r["minp"]:.2f} mean={r["meanp"]:.2f} {text}',flush=True)
    print("sec/crop",(time.time()-t0)/len(rows))
    json.dump(rows,open("torch_results.json","w"),ensure_ascii=False,indent=1)

import zlib, struct
# text PDF, two pages, hand-written
def text_pdf(path, pages):
    objs=[]; n=len(pages)
    objs.append(b"<</Type/Catalog/Pages 2 0 R>>")
    kids=" ".join(f"{4+2*i} 0 R" for i in range(n))
    objs.append(f"<</Type/Pages/Kids[{kids}]/Count {n}>>".encode())
    objs.append(b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>")
    for i,t in enumerate(pages):
        s=f"BT /F1 14 Tf 72 700 Td ({t}) Tj ET".encode()
        objs.append(f"<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Resources<</Font<</F1 3 0 R>>>>/Contents {5+2*i} 0 R>>".encode())
        objs.append(b"<</Length %d>>stream\n"%len(s)+s+b"\nendstream")
    out=b"%PDF-1.4\n"; offs=[]
    for i,o in enumerate(objs):
        offs.append(len(out)); out+=f"{i+1} 0 obj\n".encode()+o+b"\nendobj\n"
    x=len(out); out+=f"xref\n0 {len(objs)+1}\n0000000000 65535 f \n".encode()
    for o in offs: out+=f"{o:010d} 00000 n \n".encode()
    out+=f"trailer<</Size {len(objs)+1}/Root 1 0 R>>\nstartxref\n{x}\n%%EOF\n".encode()
    open(path,"wb").write(out)
text_pdf("text.pdf",["Fiber probe page one. The launch code is PELICAN-4471.","Page two. The backup colour is teal."])
# scanned: orange circle with white "73" -> PNG -> sips PDF
D={"7":["11111","00001","00010","00100","01000","01000","01000"],"3":["11110","00001","00001","01110","00001","00001","11110"]}
W=H=320; px=[[(255,255,255)]*W for _ in range(H)]
for y in range(H):
    for x in range(W):
        if (x-160)**2+(y-160)**2<140**2: px[y][x]=(240,120,20)
S=12
for k,ch in enumerate("73"):
    for r,row in enumerate(D[ch]):
        for c,b in enumerate(row):
            if b=="1":
                for dy in range(S):
                    for dx in range(S): px[100+r*S+dy][80+k*(6*S+12)+c*S+dx]=(255,255,255)
raw=b"".join(b"\0"+bytes(v for p in row for v in p) for row in px)
def ch(t,d): 
    c=struct.pack(">I",len(d))+t+d; return c+struct.pack(">I",zlib.crc32(t+d))
open("figure.png","wb").write(b"\x89PNG\r\n\x1a\n"+ch(b"IHDR",struct.pack(">IIBBBBB",W,H,8,2,0,0,0))+ch(b"IDAT",zlib.compress(raw,9))+ch(b"IEND",b""))

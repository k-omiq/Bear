//! Release matrix over real owned files, reopened native patches and exports.
use cleaner_core::{image::{self, fixtures, Format}, ingest, project::{Job,Project,StripMode}, mask::{Mask,Rect}, patch::{Patch,Provenance,Engine}, export::{self,Target}};
use std::{path::PathBuf,io::Cursor};
struct Scratch(PathBuf);
impl Drop for Scratch {fn drop(&mut self){let _=std::fs::remove_dir_all(&self.0);}}
#[test]
fn owned_original_reopen_edit_and_lossless_export_matrix() {
    let root=Scratch(std::env::temp_dir().join(format!("color-workflow-{}",std::process::id())));
    std::fs::create_dir_all(&root.0).unwrap();
    let mut cases:Vec<(String,Vec<u8>)>=fixtures::all().into_iter().map(|f| {
        let format=image::lossless_format_for(&f.raster);
        (format!("{}.{}",f.name,if format==Format::Png {"png"}else{"tif"}),image::encode(&f.raster,format).unwrap())
    }).collect();
    let references=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/color-reference");
    for name in ["cmyk-adobe-icc.jpg","cmyk-plain.jpg","ycck-adobe-icc.jpg","gamma-chrm.png","gray16-key.png","rgb16-key.png","indexed-alpha.png","modern-color.png","animated.gif","animated.webp","orientation-6.jpg"] {
        cases.push((name.into(),std::fs::read(references.join(name)).unwrap()));
    }
    for (number,(name,original)) in cases.iter().enumerate() {
        let chapter=root.0.join(number.to_string());let external=chapter.join("scans");
        std::fs::create_dir_all(&external).unwrap();let input=external.join(name);
        std::fs::write(&input,original).unwrap();
        let manifest=chapter.join("chapter.mtclean");
        let pages=chapter.join("chapter.mtclean.d/pages");
        let report=ingest::ingest_paths_importing([input],&pages);
        assert_eq!(report.sources.len(),1,"{name}: {:?}",report.skipped);
        let reference=&report.sources[0];let stored=std::fs::read(&reference.path).unwrap();
        assert_eq!(ingest::sha256_hex(&stored),reference.sha256);
        assert!(reference.path.starts_with(&pages));
        if Format::sniff(original).is_some() {assert_eq!(&stored,original,"{name}");}
        else {
            let p=reference.conversion.as_ref().expect("conversion provenance");
            assert!(p.archived_original.starts_with(chapter.join("chapter.mtclean.d/originals")));
            assert_eq!(std::fs::read(&p.archived_original).unwrap(),*original);
        }
        let project=Project::new(&chapter,"test",StripMode::Single,&report.sources);
        let job=Job::create(&manifest,project).unwrap();drop(job);
        std::fs::remove_dir_all(external).unwrap();
        let mut job=Job::open(&manifest).unwrap();let source=image::decode(&stored).unwrap();
        let before=image::proxy::display(&source).unwrap();assert_eq!(before.srgb_intent,Some(1));
        let untouched=export::export_page(&stored,&[],Target::SameAsSource).unwrap();
        assert_eq!(untouched.bytes,stored,"{name} passthrough");
        let bounds=Rect::new(0,0,source.width.min(3),source.height.min(3));
        let mut mask=Mask::empty(bounds);mask.set(0,0,true);
        let mut pixels=source.clone();pixels.width=bounds.w;pixels.height=bounds.h;pixels.data=vec![0;pixels.stride()*bounds.h as usize];
        for y in 0..bounds.h {for x in 0..bounds.w {for c in 0..source.mode.samples() {pixels.set_sample(x,y,c,source.sample(x,y,c));}}}
        let max=if source.mode==image::ColorMode::Indexed {source.palette.as_ref().unwrap().len()/3-1}else{(1usize<<source.depth.bits())-1};
        pixels.set_sample(0,0,0,if source.sample(0,0,0)==0 {max as u16}else{0});
        let patch=Patch{id:"reference-edit".into(),mask:mask.clone(),ink:mask,pixels,order:1,visible:true,
            provenance:Provenance {engine:Engine::Fill,engine_version:"test".into(),model_sha256:None,execution_provider:"cpu".into(),params_snapshot:serde_json::json!({}),mask_sha256:String::new(),source_sha256:reference.sha256.clone(),cloud:None,created:0}};
        job.complete_region(0,&patch,None).unwrap();drop(job);
        let job=Job::open(&manifest).unwrap();let patch=job.load_patch(&job.project.patches[0]).unwrap();
        let buffered=export::export_page(&stored,std::slice::from_ref(&patch),Target::SameAsSource).unwrap();
        let mut stream=Cursor::new(Vec::new());
        let summary=export::export_page_to(&stored,std::slice::from_ref(&patch),Target::SameAsSource,&mut stream).unwrap();
        assert_eq!(summary.format,buffered.format);assert_eq!(summary.declared,buffered.declared);
        let orientation=image::orientation::from_bytes(&stored);
        let size=orientation.size(source.width,source.height);
        for bytes in [&buffered.bytes,stream.get_ref()] {
            let exported=image::decode(bytes).unwrap();assert_eq!((exported.width,exported.height),size);
            let back=orientation.inverse().raster(&exported);
            assert_eq!((back.mode,back.depth),(source.mode,source.depth));
            assert_eq!(back.palette,source.palette);assert_eq!(back.trns,source.trns);
            assert_eq!(back.icc,source.icc);
            for y in 0..source.height {for x in 0..source.width {for c in 0..source.mode.samples(){
                let expected=if patch.mask.contains(x as i64,y as i64){patch.pixels.sample(x,y,c)}else{source.sample(x,y,c)};
                assert_eq!(back.sample(x,y,c),expected,"{name} {x},{y} channel {c}");
            }}}
        }
        let unchanged=std::fs::read(job.source_path(0).unwrap()).unwrap();assert_eq!(unchanged,stored,"{name} original was modified");
    }
}

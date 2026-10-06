/* SPDX-License-Identifier: CC0-1.0
 * Synthetic sources and independent libjpeg-turbo component references.
 * cc generate.c -I/opt/homebrew/include -L/opt/homebrew/lib -ljpeg -llcms2 -o /tmp/color-fixtures
 * Run /tmp/color-fixtures from this directory, then python3 generate.py.
 */
#include <stdio.h>
#include <stdlib.h>
#include <math.h>
#include <jpeglib.h>
#include <lcms2.h>
static cmsHTRANSFORM tolab, fromlab;
static int cmyk_lab(const cmsUInt16Number in[], cmsUInt16Number out[], void *unused) {
 double rgb[3], lab[3];
 for(int i=0;i<3;i++) rgb[i]=(1-in[i]/65535.0)*(1-in[3]/65535.0);
 cmsDoTransform(tolab,rgb,lab,1); cmsCIELab v={lab[0],lab[1],lab[2]}; cmsFloat2LabEncoded(out,&v); return 1;
}
static int lab_cmyk(const cmsUInt16Number in[], cmsUInt16Number out[], void *unused) {
 cmsCIELab lab; cmsLabEncoded2Float(&lab,in); double rgb[3]; cmsDoTransform(fromlab,&lab,rgb,1);
 double k=1-fmax(rgb[0],fmax(rgb[1],rgb[2]));
 for(int i=0;i<3;i++) out[i]=(cmsUInt16Number)lround(65535*(k>=1 ? 0 : (1-rgb[i]-k)/(1-k)));
 out[3]=(cmsUInt16Number)lround(k*65535); return 1;
}
static void profile(void) {
 cmsHPROFILE rgb=cmsCreate_sRGBProfile(), lab=cmsCreateLab4Profile(NULL);
 tolab=cmsCreateTransform(rgb,TYPE_RGB_DBL,lab,TYPE_Lab_DBL,INTENT_RELATIVE_COLORIMETRIC,0);
 fromlab=cmsCreateTransform(lab,TYPE_Lab_DBL,rgb,TYPE_RGB_DBL,INTENT_RELATIVE_COLORIMETRIC,0);
 cmsHPROFILE p=cmsCreateProfilePlaceholder(NULL);
 cmsSetProfileVersion(p,4.3); cmsSetDeviceClass(p,cmsSigOutputClass); cmsSetColorSpace(p,cmsSigCmykData); cmsSetPCS(p,cmsSigLabData);
 cmsSetHeaderRenderingIntent(p,INTENT_RELATIVE_COLORIMETRIC);
 cmsWriteTag(p,cmsSigMediaWhitePointTag,cmsD50_XYZ());
 cmsMLU *text=cmsMLUalloc(NULL,1); cmsMLUsetASCII(text,"en","US","Synthetic CMYK analytic ink model; CC0");
 cmsWriteTag(p,cmsSigProfileDescriptionTag,text); cmsWriteTag(p,cmsSigCopyrightTag,text); cmsMLUfree(text);
 for(int direction=0;direction<2;direction++) {
   int in=direction?3:4,out=direction?4:3;
   cmsPipeline *pipe=cmsPipelineAlloc(NULL,in,out);
   cmsToneCurve *curves[4]; for(int i=0;i<4;i++)curves[i]=cmsBuildGamma(NULL,1);
   cmsPipelineInsertStage(pipe,cmsAT_END,cmsStageAllocToneCurves(NULL,in,curves));
   cmsStage *clut=cmsStageAllocCLut16bit(NULL,9,in,out,NULL);
   cmsStageSampleCLut16bit(clut,direction?lab_cmyk:cmyk_lab,NULL,0);
   cmsPipelineInsertStage(pipe,cmsAT_END,clut);
   cmsPipelineInsertStage(pipe,cmsAT_END,cmsStageAllocToneCurves(NULL,out,curves));
   cmsWriteTag(p,direction?cmsSigBToA0Tag:cmsSigAToB0Tag,pipe);
   cmsPipelineFree(pipe); for(int i=0;i<4;i++)cmsFreeToneCurve(curves[i]);
 }
 if(!cmsSaveProfileToFile(p,"synthetic-cmyk.icc"))exit(1);
 cmsCloseProfile(p); cmsDeleteTransform(tolab); cmsDeleteTransform(fromlab); cmsCloseProfile(rgb); cmsCloseProfile(lab);
}
static void jpeg(const char *name, int ycck, int adobe, int subsample) {
 struct jpeg_compress_struct c; struct jpeg_error_mgr e; c.err=jpeg_std_error(&e); jpeg_create_compress(&c);
 FILE *f=fopen(name,"wb"); jpeg_stdio_dest(&c,f); c.image_width=16;c.image_height=8;c.input_components=4;c.in_color_space=JCS_CMYK;
 jpeg_set_defaults(&c); jpeg_set_colorspace(&c,ycck?JCS_YCCK:JCS_CMYK); c.write_Adobe_marker=adobe;
 for(int i=0;i<4;i++) c.comp_info[i].h_samp_factor=c.comp_info[i].v_samp_factor=1;
 if(subsample) { c.comp_info[0].h_samp_factor=c.comp_info[0].v_samp_factor=2; c.comp_info[3].h_samp_factor=c.comp_info[3].v_samp_factor=2; }
 jpeg_set_quality(&c,95,TRUE);jpeg_start_compress(&c,TRUE);
 unsigned char row[64];while(c.next_scanline<c.image_height) {
   for(int x=0;x<16;x++)for(int ch=0;ch<4;ch++){
     int native=(x*13+c.next_scanline*17+ch*43)%256;
     row[x*4+ch]=adobe?255-native:native;
   }
   JSAMPROW ptr=row;jpeg_write_scanlines(&c,&ptr,1);
 }
 jpeg_finish_compress(&c);jpeg_destroy_compress(&c);fclose(f);
 struct jpeg_decompress_struct d;d.err=jpeg_std_error(&e);jpeg_create_decompress(&d);f=fopen(name,"rb");jpeg_stdio_src(&d,f);jpeg_read_header(&d,TRUE);
 d.out_color_space=JCS_CMYK;d.dct_method=JDCT_ISLOW;d.do_fancy_upsampling=subsample;jpeg_start_decompress(&d);
 char reference[100];snprintf(reference,sizeof(reference),"%s.cmyk",name);FILE *o=fopen(reference,"wb");
 while(d.output_scanline<d.output_height){JSAMPROW ptr=row;jpeg_read_scanlines(&d,&ptr,1);if(adobe)for(int i=0;i<64;i++)row[i]=255-row[i];fwrite(row,1,64,o);}
 fclose(o);jpeg_finish_decompress(&d);jpeg_destroy_decompress(&d);fclose(f);
}
int main(void){ profile();jpeg("cmyk-adobe.jpg",0,1,0);jpeg("cmyk-plain.jpg",0,0,0);jpeg("ycck-adobe.jpg",1,1,0);jpeg("ycck-subsampled.jpg",1,1,1);jpeg("ycck-markerless.jpg",1,0,0);return 0;}

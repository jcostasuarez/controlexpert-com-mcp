param([string]$OutputPath = (Join-Path $env:TEMP 'controlexpert_typelibs_raw.json'))
$ErrorActionPreference = 'Stop'
# Reads type metadata only: does not instantiate Control Expert or connect to a PLC.
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
// .NET Framework also defines obsolete copies of these types in InteropServices.
using TYPEDESC = System.Runtime.InteropServices.ComTypes.TYPEDESC;
using TYPEATTR = System.Runtime.InteropServices.ComTypes.TYPEATTR;
using TYPELIBATTR = System.Runtime.InteropServices.ComTypes.TYPELIBATTR;
using FUNCDESC = System.Runtime.InteropServices.ComTypes.FUNCDESC;
using ELEMDESC = System.Runtime.InteropServices.ComTypes.ELEMDESC;
using VARDESC = System.Runtime.InteropServices.ComTypes.VARDESC;
using PARAMFLAG = System.Runtime.InteropServices.ComTypes.PARAMFLAG;
using FUNCFLAGS = System.Runtime.InteropServices.ComTypes.FUNCFLAGS;
using VARFLAGS = System.Runtime.InteropServices.ComTypes.VARFLAGS;
using VARKIND = System.Runtime.InteropServices.ComTypes.VARKIND;
using TYPEKIND = System.Runtime.InteropServices.ComTypes.TYPEKIND;
using IMPLTYPEFLAGS = System.Runtime.InteropServices.ComTypes.IMPLTYPEFLAGS;
public static class TypeLibReader {
 [DllImport("oleaut32.dll", CharSet=CharSet.Unicode, PreserveSig=false)]
 static extern void LoadTypeLibEx(string file, int mode, out ITypeLib lib);
 static Dictionary<string,object> D(params object[] a) {
  var d=new Dictionary<string,object>(); for(int i=0;i<a.Length;i+=2)d[(string)a[i]]=a[i+1]; return d;
 }
 static object Doc(ITypeInfo t, int id) {
  string n,s,h; int c; t.GetDocumentation(id,out n,out s,out c,out h);
  return D("name",n,"text",s,"help_file",h,"help_context",c);
 }
 static object Type(ITypeInfo t,TYPEDESC d,int depth=0) {
  var vt=(VarEnum)d.vt;
  if(depth>12) return D("kind",vt.ToString(),"unresolved",true);
  if(vt==VarEnum.VT_PTR || vt==VarEnum.VT_SAFEARRAY) return D("kind",vt.ToString(),"element",Type(t,(TYPEDESC)Marshal.PtrToStructure(d.lpValue,typeof(TYPEDESC)),depth+1));
  if(vt==VarEnum.VT_USERDEFINED) {
   ITypeInfo r; t.GetRefTypeInfo(unchecked((int)d.lpValue.ToInt64()),out r); IntPtr p; r.GetTypeAttr(out p);
   try { var a=(TYPEATTR)Marshal.PtrToStructure(p,typeof(TYPEATTR)); string n,s,h; int c; r.GetDocumentation(-1,out n,out s,out c,out h);
    ITypeLib rl; int ri; r.GetContainingTypeLib(out rl,out ri); string ln,ls,lh; int lc; rl.GetDocumentation(-1,out ln,out ls,out lc,out lh);
    return D("kind",vt.ToString(),"name",n,"guid",a.guid.ToString("B"),"type_kind",a.typekind.ToString(),"library",ln);
   }
   finally {r.ReleaseTypeAttr(p);}
  }
  return D("kind",vt.ToString());
 }
 public static object Read(string file) {
  ITypeLib lib; LoadTypeLibEx(file,2,out lib); // REGKIND_NONE
  string name,doc,help; int hc; lib.GetDocumentation(-1,out name,out doc,out hc,out help);
  IntPtr lp; lib.GetLibAttr(out lp); TYPELIBATTR la;
  try {la=(TYPELIBATTR)Marshal.PtrToStructure(lp,typeof(TYPELIBATTR));} finally {lib.ReleaseTLibAttr(lp);}
  var types=new List<object>();
  for(int i=0;i<lib.GetTypeInfoCount();i++) {
   ITypeInfo t; lib.GetTypeInfo(i,out t); IntPtr ap; t.GetTypeAttr(out ap);
   try {
    var a=(TYPEATTR)Marshal.PtrToStructure(ap,typeof(TYPEATTR));
    var funcs=new List<object>(); var vars=new List<object>(); var impls=new List<object>();
    for(int j=0;j<a.cFuncs;j++) {
     IntPtr fp; t.GetFuncDesc(j,out fp);
     try {
      var f=(FUNCDESC)Marshal.PtrToStructure(fp,typeof(FUNCDESC));
      string[] names=new string[f.cParams+1]; int nc; t.GetNames(f.memid,names,names.Length,out nc);
      var pars=new List<object>();
      for(int k=0;k<f.cParams;k++) {
       var e=(ELEMDESC)Marshal.PtrToStructure(IntPtr.Add(f.lprgelemdescParam,k*Marshal.SizeOf(typeof(ELEMDESC))),typeof(ELEMDESC));
       var p=D("name", k+1<nc ? names[k+1] : "arg"+k,"position",k,"type",Type(t,e.tdesc),"flags",e.desc.paramdesc.wParamFlags.ToString(),"flags_value",(int)e.desc.paramdesc.wParamFlags);
       if((e.desc.paramdesc.wParamFlags & PARAMFLAG.PARAMFLAG_FHASDEFAULT)!=0 && e.desc.paramdesc.lpVarValue!=IntPtr.Zero) {
        try {p["default"]=Marshal.GetObjectForNativeVariant(IntPtr.Add(e.desc.paramdesc.lpVarValue,8));} catch(Exception ex){p["default_read_error"]=ex.Message;}
       }
       pars.Add(p);
      }
      funcs.Add(D("index",j,"documentation",Doc(t,f.memid),"dispid",f.memid,"invoke_kind",f.invkind.ToString(),"function_kind",f.funckind.ToString(),"flags",((FUNCFLAGS)f.wFuncFlags).ToString(),"flags_value",f.wFuncFlags,"optional_parameter_count",f.cParamsOpt,"vtable_offset",f.oVft,"calling_convention",f.callconv.ToString(),"return_type",Type(t,f.elemdescFunc.tdesc),"parameters",pars));
     } finally {t.ReleaseFuncDesc(fp);}
    }
    for(int j=0;j<a.cVars;j++) {
     IntPtr vp; t.GetVarDesc(j,out vp);
     try {var v=(VARDESC)Marshal.PtrToStructure(vp,typeof(VARDESC)); object val=null;
      if(v.varkind==VARKIND.VAR_CONST) val=Marshal.GetObjectForNativeVariant(v.desc.lpvarValue);
      vars.Add(D("documentation",Doc(t,v.memid),"type",Type(t,v.elemdescVar.tdesc),"value",val,"kind",v.varkind.ToString(),"flags",((VARFLAGS)v.wVarFlags).ToString()));
     } finally {t.ReleaseVarDesc(vp);}
    }
    for(int j=0;j<a.cImplTypes;j++) {
     int r; IMPLTYPEFLAGS flags; t.GetRefTypeOfImplType(j,out r); t.GetImplTypeFlags(j,out flags); ITypeInfo rt; t.GetRefTypeInfo(r,out rt);
     string rn,rs,rh; int rc; rt.GetDocumentation(-1,out rn,out rs,out rc,out rh); IntPtr rp; rt.GetTypeAttr(out rp);
     try {var ra=(TYPEATTR)Marshal.PtrToStructure(rp,typeof(TYPEATTR)); impls.Add(D("name",rn,"guid",ra.guid.ToString("B"),"flags",flags.ToString()));} finally {rt.ReleaseTypeAttr(rp);}
    }
    var td=D("index",i,"documentation",Doc(t,-1),"guid",a.guid.ToString("B"),"kind",a.typekind.ToString(),"flags",a.wTypeFlags.ToString(),"functions",funcs,"variables",vars,"implemented_types",impls,"declared_function_count",a.cFuncs,"declared_variable_count",a.cVars);
    if(a.typekind==TYPEKIND.TKIND_ALIAS) td["alias_type"]=Type(t,a.tdescAlias);
    types.Add(td);
   } finally {t.ReleaseTypeAttr(ap);}
  }
  return D("path",file,"name",name,"documentation",doc,"help_file",help,"guid",la.guid.ToString("B"),"version",la.wMajorVerNum+"."+la.wMinorVerNum,"lcid",la.lcid,"platform",la.syskind.ToString(),"types",types);
 }
}
'@
$base = 'C:\Program Files (x86)\Schneider Electric\Control Expert 15.1'
$shared = 'C:\Program Files (x86)\Common Files\Schneider Electric Shared\Tlb'
$paths = @('PSBroker.tlb','PServer.tlb','PSExe.tlb','PSNotify.tlb','PSRDtm.tlb','LibsetServer.tlb','SecurityServer.tlb','SecurityServerInternal.tlb','UnityFdtCatalogServer.tlb') | ForEach-Object { Join-Path $shared $_ }
$paths += @((Join-Path $base 'PLC_Simulator\Logger.tlb'), (Join-Path $base 'CsConfigMgr.tlb'))
# Additional embedded type libraries are exposed as numbered resources.
$paths += 1..8 | ForEach-Object { (Join-Path $base 'pserver.dll') + '\' + $_ }
$libs = @(); $failures = @(); $seen = @{}
foreach ($path in $paths) {
 try {
  $lib = [TypeLibReader]::Read($path)
  $key = $lib['guid'] + ':' + $lib['version']
  if (-not $seen.ContainsKey($key)) { $libs += $lib; $seen[$key]=$lib }
  else { $seen[$key]['alternate_paths'] = @($seen[$key]['alternate_paths']) + $path }
 } catch { $failures += @{ path=$path; error=$_.Exception.Message } }
}
$registration = @{}
foreach ($view in @([Microsoft.Win32.RegistryView]::Registry32,[Microsoft.Win32.RegistryView]::Registry64)) {
 $root = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::ClassesRoot,$view)
 foreach ($lib in $libs) { foreach ($type in $lib['types']) { if ($type['kind'] -eq 'TKIND_COCLASS') {
  $id=$type['guid']; $key=$root.OpenSubKey("CLSID\$id")
  if ($null -ne $key) {
   $values=@{view=$view.ToString()}
   foreach ($child in @('ProgID','VersionIndependentProgID','LocalServer32','InprocServer32','TypeLib')) {
    $sub=$key.OpenSubKey($child)
    if ($null -ne $sub) { $values[$child]=$sub.GetValue(''); $sub.Dispose() }
   }
   $registration[$id] = @($registration[$id]) + $values
   $key.Dispose()
  }
 } } }
 $root.Dispose()
}
@{libraries=$libs; discovery_failures=$failures; registration=$registration} | ConvertTo-Json -Depth 80 | Set-Content -LiteralPath $OutputPath -Encoding utf8
Write-Output "Extracted $($libs.Count) libraries; $($failures.Count) unavailable resource candidates. Output: $OutputPath"

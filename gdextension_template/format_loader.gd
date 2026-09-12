@tool
class_name VLCMediaFormatLoader extends ResourceFormatLoader

var extensions: PackedStringArray = \
# audio
[
"3ga", "669", "a52", "acc", "ac3", "adt", "adts", "aif", "aifc", "aiff", "alac", "amr", "aob",\
"au", "ape", "caf", "cda", "dts", "dsf", "dff", "flac", "it", "m4a", "m4p", "mka", "mlp", "mod",\
"mp1", "mp2", "mp3", "mpc", "mpga", "oga", "oma", "opus", "qcp", "ra", "rmi", "snd", "s3m", "spx",\
"tak", "tta", "voc", "vqf", "w64", "wav", "wma", "wv", "xa", "xm",\
# video
"3g2", "3gp", "3gp2", "3gpp", "amrec", "amv", "asf", "avi", "bik", "dav", "divx", "drc", "dv",\
"dvr-ms", "evo", "f4v", "flv", "gvi", "gxf", "k3g", "m1v", "m2t", "m2v", "m2ts", "m4v", "mkv",\
"mov", "mp2v", "mp4", "mp4v", "mpa", "mpe", "mpeg", "mpeg1", "mpeg2", "mpeg4", "mpg", "mpv2",\
"mts", "mtv", "mxf", "nsv", "nuv", "ogg", "ogm", "ogx", "ogv", "qt", "rec", "rm", "rmvb", "rpl",\
"skm", "thp", "tod", "tp", "ts", "tts", "vob", "vp6", "vro", "webm", "wmv", "wtv", ".xesc"
]

func _get_recognized_extensions() -> PackedStringArray:
	return extensions

func _get_resource_type(path: String) -> String:
	var extension = path.get_extension()
	if extensions.has(extension):
		return "VLCMedia"
	else:
		return ""

func _handles_type(type: StringName) -> bool:
	return true

func _load(path: String, _original_path: String, _use_sub_threads: bool, _cache_mode: int) -> Variant:
	var f := FileAccess.open(path, FileAccess.READ)
	if f == null:
		return ERR_CANT_OPEN
	f.close()
	var resource := VLCMedia.load_from_file(path)
	return resource

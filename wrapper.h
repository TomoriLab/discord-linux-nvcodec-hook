#include <ffnvcodec/dynlink_cuda.h>
#include <ffnvcodec/dynlink_nvcuvid.h>

CUresult cuInit(unsigned int flags);
CUresult cuDeviceGet(CUdevice *device, int ordinal);
CUresult cuCtxCreate_v2(CUcontext *context, unsigned int flags, CUdevice device);
CUresult cuCtxDestroy_v2(CUcontext context);
CUresult cuCtxPushCurrent_v2(CUcontext context);
CUresult cuCtxPopCurrent_v2(CUcontext *context);
CUresult cuMemcpy2D_v2(const CUDA_MEMCPY2D *copy);

CUresult cuvidCreateVideoParser(CUvideoparser *parser, CUVIDPARSERPARAMS *parameters);
CUresult cuvidDestroyVideoParser(CUvideoparser parser);
CUresult cuvidParseVideoData(CUvideoparser parser, CUVIDSOURCEDATAPACKET *packet);
CUresult cuvidCreateDecoder(CUvideodecoder *decoder, CUVIDDECODECREATEINFO *information);
CUresult cuvidDestroyDecoder(CUvideodecoder decoder);
CUresult cuvidDecodePicture(CUvideodecoder decoder, CUVIDPICPARAMS *picture);
CUresult cuvidMapVideoFrame64(CUvideodecoder decoder, int index, unsigned long long *pointer,
                             unsigned int *pitch, CUVIDPROCPARAMS *parameters);
CUresult cuvidUnmapVideoFrame64(CUvideodecoder decoder, unsigned long long pointer);
CUresult cuvidGetDecoderCaps(CUVIDDECODECAPS *capabilities);

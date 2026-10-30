#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreAudio/CoreAudioTypes.h>
#import <CoreGraphics/CoreGraphics.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <stdatomic.h>

typedef void (*WhisperAudioCallback)(const float *samples, size_t count, void *user_data);

static atomic_uint_fast64_t g_audio_packets = 0;
static atomic_uint_fast64_t g_video_packets = 0;
static atomic_uint_fast64_t g_audio_bytes = 0;
static atomic_int_fast32_t g_last_status = 0;
static atomic_uint_fast32_t g_sample_rate = 0;
static atomic_uint_fast32_t g_channels = 0;
static atomic_uint_fast32_t g_bits_per_channel = 0;

uint64_t whisper_macos_audio_packets_received(void) {
    return atomic_load(&g_audio_packets);
}

uint64_t whisper_macos_video_packets_received(void) {
    return atomic_load(&g_video_packets);
}

uint64_t whisper_macos_audio_bytes_received(void) {
    return atomic_load(&g_audio_bytes);
}

int32_t whisper_macos_audio_last_status(void) {
    return atomic_load(&g_last_status);
}

uint32_t whisper_macos_audio_sample_rate(void) {
    return atomic_load(&g_sample_rate);
}

@interface WhisperSCStreamHandler : NSObject <SCStreamOutput, SCStreamDelegate>
@property (nonatomic, assign) WhisperAudioCallback callback;
@property (nonatomic, assign) void *userData;
@property (nonatomic, strong) SCStream *stream;
@property (nonatomic, strong) dispatch_queue_t audioQueue;
@property (nonatomic, strong) dispatch_queue_t videoQueue;
@end

@implementation WhisperSCStreamHandler

- (void)stream:(SCStream *)stream
didOutputSampleBuffer:(CMSampleBufferRef)sampleBuffer
        ofType:(SCStreamOutputType)type {
    if (type == SCStreamOutputTypeScreen) {
        atomic_fetch_add(&g_video_packets, 1);
        return;
    }
    if (type != SCStreamOutputTypeAudio) {
        return;
    }

    atomic_fetch_add(&g_audio_packets, 1);

    if (!sampleBuffer || !CMSampleBufferDataIsReady(sampleBuffer)) {
        return;
    }

    CMItemCount numSamples = CMSampleBufferGetNumSamples(sampleBuffer);
    if (numSamples <= 0 || !self.callback) {
        return;
    }

    CMFormatDescriptionRef formatDesc = CMSampleBufferGetFormatDescription(sampleBuffer);
    if (!formatDesc) {
        return;
    }
    const AudioStreamBasicDescription *asbd = CMAudioFormatDescriptionGetStreamBasicDescription(formatDesc);
    if (!asbd || asbd->mFormatID != kAudioFormatLinearPCM) {
        return;
    }

    double srcSampleRate = asbd->mSampleRate > 0 ? asbd->mSampleRate : 16000.0;
    atomic_store(&g_sample_rate, (uint32_t)srcSampleRate);
    atomic_store(&g_channels, (uint32_t)asbd->mChannelsPerFrame);
    atomic_store(&g_bits_per_channel, (uint32_t)asbd->mBitsPerChannel);

    // Query required AudioBufferList size first
    size_t requiredSize = 0;
    CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sampleBuffer,
        &requiredSize,
        NULL,
        0,
        NULL,
        NULL,
        0,
        NULL
    );

    size_t ablSize = requiredSize > 0
        ? requiredSize
        : (sizeof(AudioBufferList) + sizeof(AudioBuffer) * 7);
    AudioBufferList *abl = (AudioBufferList *)alloca(ablSize);
    memset(abl, 0, ablSize);

    CMBlockBufferRef blockBuffer = NULL;
    OSStatus status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sampleBuffer,
        NULL,
        abl,
        ablSize,
        kCFAllocatorDefault,
        kCFAllocatorDefault,
        0, // Do NOT require 16-byte alignment (ScreenCaptureKit buffers are often unaligned)
        &blockBuffer
    );

    atomic_store(&g_last_status, (int32_t)status);

    if (status != noErr || !blockBuffer) {
        // Fallback: copy directly from CMBlockBuffer if AudioBufferList extraction failed
        CMBlockBufferRef rawBlock = CMSampleBufferGetDataBuffer(sampleBuffer);
        if (!rawBlock) {
            return;
        }
        size_t totalLen = CMBlockBufferGetDataLength(rawBlock);
        if (totalLen == 0) {
            return;
        }
        void *rawBuf = malloc(totalLen);
        if (!rawBuf) {
            return;
        }
        if (CMBlockBufferCopyDataBytes(rawBlock, 0, totalLen, rawBuf) == kCMBlockBufferNoErr) {
            atomic_fetch_add(&g_audio_bytes, (uint64_t)totalLen);
            size_t floatCount = totalLen / sizeof(float);
            if (floatCount > 0) {
                self.callback((const float *)rawBuf, floatCount, self.userData);
            }
        }
        free(rawBuf);
        return;
    }

    uint64_t packetBytes = 0;
    for (UInt32 b = 0; b < abl->mNumberBuffers; b++) {
        packetBytes += abl->mBuffers[b].mDataByteSize;
    }
    atomic_fetch_add(&g_audio_bytes, packetBytes);

    BOOL isFloat = (asbd->mFormatFlags & kAudioFormatFlagIsFloat) != 0;
    BOOL isSignedInt = (asbd->mFormatFlags & kAudioFormatFlagIsSignedInteger) != 0;
    BOOL isNonInterleaved = (asbd->mFormatFlags & kAudioFormatFlagIsNonInterleaved) != 0;
    UInt32 channels = asbd->mChannelsPerFrame > 0 ? asbd->mChannelsPerFrame : 1;
    UInt32 bitsPerChannel = asbd->mBitsPerChannel;

    size_t frames = (size_t)numSamples;
    float *mono = (float *)malloc(frames * sizeof(float));
    if (!mono) {
        CFRelease(blockBuffer);
        return;
    }
    memset(mono, 0, frames * sizeof(float));

    if (isFloat && bitsPerChannel == 32) {
        if (channels == 1 && abl->mNumberBuffers >= 1 && abl->mBuffers[0].mData) {
            size_t avail = abl->mBuffers[0].mDataByteSize / sizeof(float);
            size_t n = avail < frames ? avail : frames;
            memcpy(mono, abl->mBuffers[0].mData, n * sizeof(float));
            frames = n;
        } else if (isNonInterleaved) {
            UInt32 numBufs = abl->mNumberBuffers;
            for (UInt32 b = 0; b < numBufs; b++) {
                const float *chData = (const float *)abl->mBuffers[b].mData;
                size_t chFrames = abl->mBuffers[b].mDataByteSize / sizeof(float);
                size_t n = chFrames < frames ? chFrames : frames;
                if (chData) {
                    for (size_t i = 0; i < n; i++) {
                        mono[i] += chData[i] / (float)numBufs;
                    }
                }
            }
        } else if (abl->mNumberBuffers >= 1 && abl->mBuffers[0].mData) {
            const float *interleaved = (const float *)abl->mBuffers[0].mData;
            size_t totalFloats = abl->mBuffers[0].mDataByteSize / sizeof(float);
            size_t availFrames = totalFloats / channels;
            size_t n = availFrames < frames ? availFrames : frames;
            for (size_t i = 0; i < n; i++) {
                float sum = 0.0f;
                for (UInt32 c = 0; c < channels; c++) {
                    sum += interleaved[i * channels + c];
                }
                mono[i] = sum / (float)channels;
            }
            frames = n;
        }
    } else if (isSignedInt && bitsPerChannel == 16 && abl->mNumberBuffers >= 1 && abl->mBuffers[0].mData) {
        const int16_t *pcm16 = (const int16_t *)abl->mBuffers[0].mData;
        size_t totalSamples = abl->mBuffers[0].mDataByteSize / sizeof(int16_t);
        size_t availFrames = isNonInterleaved ? totalSamples : (totalSamples / channels);
        size_t n = availFrames < frames ? availFrames : frames;
        for (size_t i = 0; i < n; i++) {
            if (isNonInterleaved) {
                mono[i] = (float)pcm16[i] / 32768.0f;
            } else {
                float sum = 0.0f;
                for (UInt32 c = 0; c < channels; c++) {
                    sum += (float)pcm16[i * channels + c] / 32768.0f;
                }
                mono[i] = sum / (float)channels;
            }
        }
        frames = n;
    }

    // Resample to 16000 Hz if ScreenCaptureKit delivered a different sample rate
    if (frames > 0 && fabs(srcSampleRate - 16000.0) > 1.0) {
        double ratio = srcSampleRate / 16000.0;
        size_t outFrames = (size_t)((double)frames / ratio);
        if (outFrames > 0) {
            float *resampled = (float *)malloc(outFrames * sizeof(float));
            if (resampled) {
                for (size_t i = 0; i < outFrames; i++) {
                    double srcIdx = (double)i * ratio;
                    size_t idx0 = (size_t)srcIdx;
                    size_t idx1 = (idx0 + 1 < frames) ? (idx0 + 1) : idx0;
                    float frac = (float)(srcIdx - (double)idx0);
                    resampled[i] = mono[idx0] * (1.0f - frac) + mono[idx1] * frac;
                }
                self.callback(resampled, outFrames, self.userData);
                free(resampled);
            }
        }
    } else if (frames > 0) {
        self.callback(mono, frames, self.userData);
    }

    free(mono);
    CFRelease(blockBuffer);
}

- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error {
    if (error) {
        fprintf(stderr, "\n[macOS Audio] Stream stopped with error: %s\n",
                [[error localizedDescription] UTF8String]);
    }
}

@end

static char *dup_cstr(NSString *str) {
    if (!str) return NULL;
    const char *utf8 = [str UTF8String];
    if (!utf8) return NULL;
    return strdup(utf8);
}

void *whisper_macos_audio_start(WhisperAudioCallback callback, void *user_data, char **error_out) {
    @autoreleasepool {
        atomic_store(&g_audio_packets, 0);
        atomic_store(&g_video_packets, 0);
        atomic_store(&g_audio_bytes, 0);
        atomic_store(&g_last_status, 0);

        // Initialize CoreGraphics WindowServer connection for CLI process
        (void)CGMainDisplayID();

        if (!CGPreflightScreenCaptureAccess()) {
            CGRequestScreenCaptureAccess();
        }

        __block SCShareableContent *content = nil;
        __block NSError *shareError = nil;
        dispatch_semaphore_t sem = dispatch_semaphore_create(0);

        [SCShareableContent getShareableContentExcludingDesktopWindows:NO
                                                   onScreenWindowsOnly:NO
                                                     completionHandler:^(SCShareableContent * _Nullable shareableContent, NSError * _Nullable error) {
            content = shareableContent;
            shareError = error;
            dispatch_semaphore_signal(sem);
        }];

        dispatch_semaphore_wait(sem, DISPATCH_TIME_FOREVER);

        if (shareError || !content) {
            if (error_out) {
                NSString *msg = shareError
                    ? [NSString stringWithFormat:@"ScreenCaptureKit permission/access error: %@. Please grant 'Screen & System Audio Recording' permission to your terminal in System Settings -> Privacy & Security.", [shareError localizedDescription]]
                    : @"Failed to retrieve shareable content from macOS ScreenCaptureKit.";
                *error_out = dup_cstr(msg);
            }
            return NULL;
        }

        SCDisplay *display = content.displays.firstObject;
        if (!display) {
            if (error_out) {
                *error_out = dup_cstr(@"No active display found for ScreenCaptureKit audio capture.");
            }
            return NULL;
        }

        SCContentFilter *filter = [[SCContentFilter alloc] initWithDisplay:display excludingWindows:@[]];

        SCStreamConfiguration *config = [[SCStreamConfiguration alloc] init];
        config.capturesAudio = YES;
        config.sampleRate = 16000;
        config.channelCount = 1;
        config.excludesCurrentProcessAudio = NO;
        config.width = 64;
        config.height = 64;
        config.minimumFrameInterval = CMTimeMake(1, 5);
        config.showsCursor = NO;

        WhisperSCStreamHandler *handler = [[WhisperSCStreamHandler alloc] init];
        handler.callback = callback;
        handler.userData = user_data;
        handler.audioQueue = dispatch_queue_create("com.whispercli.audio", DISPATCH_QUEUE_SERIAL);
        handler.videoQueue = dispatch_queue_create("com.whispercli.video", DISPATCH_QUEUE_SERIAL);

        SCStream *stream = [[SCStream alloc] initWithFilter:filter configuration:config delegate:handler];
        handler.stream = stream;

        NSError *addOutputError = nil;
        [stream addStreamOutput:handler
                           type:SCStreamOutputTypeScreen
             sampleHandlerQueue:handler.videoQueue
                          error:&addOutputError];
        addOutputError = nil;

        BOOL addedAudio = [stream addStreamOutput:handler
                                             type:SCStreamOutputTypeAudio
                               sampleHandlerQueue:handler.audioQueue
                                            error:&addOutputError];
        if (!addedAudio) {
            if (error_out) {
                NSString *msg = [NSString stringWithFormat:@"Failed to attach audio stream output: %@",
                                 [addOutputError localizedDescription]];
                *error_out = dup_cstr(msg);
            }
            return NULL;
        }

        __block NSError *startError = nil;
        dispatch_semaphore_t startSem = dispatch_semaphore_create(0);
        [stream startCaptureWithCompletionHandler:^(NSError * _Nullable error) {
            startError = error;
            dispatch_semaphore_signal(startSem);
        }];
        dispatch_semaphore_wait(startSem, DISPATCH_TIME_FOREVER);

        if (startError) {
            if (error_out) {
                NSString *msg = [NSString stringWithFormat:@"Failed to start macOS system audio capture: %@. Ensure 'Screen & System Audio Recording' is enabled for your terminal in System Settings -> Privacy & Security.",
                                 [startError localizedDescription]];
                *error_out = dup_cstr(msg);
            }
            return NULL;
        }

        return (__bridge_retained void *)handler;
    }
}

void whisper_macos_audio_stop(void *handle) {
    if (!handle) return;
    @autoreleasepool {
        WhisperSCStreamHandler *handler = (__bridge_transfer WhisperSCStreamHandler *)handle;
        handler.callback = NULL;
        if (handler.stream) {
            dispatch_semaphore_t stopSem = dispatch_semaphore_create(0);
            [handler.stream stopCaptureWithCompletionHandler:^(NSError * _Nullable error) {
                dispatch_semaphore_signal(stopSem);
            }];
            dispatch_time_t timeout = dispatch_time(DISPATCH_TIME_NOW, (int64_t)(2 * NSEC_PER_SEC));
            dispatch_semaphore_wait(stopSem, timeout);
            handler.stream = nil;
        }
    }
}

void whisper_macos_free_error(char *err) {
    if (err) {
        free(err);
    }
}

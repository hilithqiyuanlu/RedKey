#import <AVFoundation/AVFoundation.h>
#import <dispatch/dispatch.h>

int redkey_microphone_authorization_status(void) {
    return (int)[AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio];
}

static void request_microphone_permission_on_main_thread(void) {
    if ([AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio] !=
        AVAuthorizationStatusNotDetermined) {
        return;
    }
    [AVCaptureDevice requestAccessForMediaType:AVMediaTypeAudio
                             completionHandler:^(__unused BOOL allowed) {
    }];
}

void redkey_request_microphone_permission(void) {
    if ([NSThread isMainThread]) {
        request_microphone_permission_on_main_thread();
    } else {
        dispatch_async(dispatch_get_main_queue(), ^{
            request_microphone_permission_on_main_thread();
        });
    }
}

package io.streambeam.streambeam

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.util.DisplayMetrics
import android.view.Surface
import android.view.WindowManager

/**
 * Captures the screen via MediaProjection, encodes it to H.264 with MediaCodec,
 * and pushes the encoded access units into the Rust host encoder
 * (`streambeam_host::android_screen`) through [NativeStream]. The StreamBeam
 * QUIC host server then streams them to a paired client, unchanged.
 *
 * Runs as a foreground service so it survives while you use other apps, and
 * raises notifications for the ongoing beam and for incoming access requests
 * (which would otherwise be invisible while the app is backgrounded).
 */
class ScreenCaptureService : Service() {
    private var projection: MediaProjection? = null
    private var codec: MediaCodec? = null
    private var inputSurface: Surface? = null
    private var virtualDisplay: VirtualDisplay? = null
    private var drainThread: Thread? = null
    private var displayListener: DisplayManager.DisplayListener? = null

    @Volatile
    private var running = false

    /** Current encode size; swaps on device rotation (see [maybeRestartForRotation]). */
    @Volatile
    private var captureWidth = 0

    @Volatile
    private var captureHeight = 0

    @Volatile
    private var captureDpi = 0

    private val approvalHandler = Handler(Looper.getMainLooper())
    private var shownApprovalId: Long = -1
    private var approvalPoll: Runnable? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                stopSelf()
                return START_NOT_STICKY
            }
            ACTION_ALLOW, ACTION_DENY -> {
                val id = intent.getLongExtra(EXTRA_APPROVAL_ID, -1)
                if (id >= 0) {
                    NativeStream.nativeRespondPairing(id, intent.action == ACTION_ALLOW)
                }
                notificationManager().cancel(APPROVAL_NOTIF_ID)
                shownApprovalId = -1
                return START_STICKY
            }
        }

        if (running) return START_STICKY
        val resultCode = intent?.getIntExtra(EXTRA_RESULT_CODE, 0) ?: 0
        val data = intent?.getParcelableExtra<Intent>(EXTRA_RESULT_DATA)
        if (resultCode == 0 || data == null) {
            stopSelf()
            return START_NOT_STICKY
        }

        startAsForeground()
        try {
            startCapture(resultCode, data)
            captureActive = true
            remoteCaptureActive = intent.getBooleanExtra(EXTRA_REMOTE_REQUEST, false)
        } catch (e: Exception) {
            android.util.Log.e(TAG, "screen capture failed to start", e)
            stopSelf()
            return START_NOT_STICKY
        }
        startApprovalPolling()
        return START_STICKY
    }

    private fun startCapture(resultCode: Int, data: Intent) {
        val metrics = DisplayMetrics()
        val wm = getSystemService(Context.WINDOW_SERVICE) as WindowManager
        @Suppress("DEPRECATION")
        wm.defaultDisplay.getRealMetrics(metrics)
        val (w, h) = scaledSize(metrics.widthPixels, metrics.heightPixels)
        captureWidth = w
        captureHeight = h
        captureDpi = metrics.densityDpi

        val mpm = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        val proj = mpm.getMediaProjection(resultCode, data)
            ?: throw IllegalStateException("MediaProjection unavailable")
        projection = proj
        proj.registerCallback(object : MediaProjection.Callback() {
            override fun onStop() {
                stopSelf()
            }
        }, null)

        NativeStream.nativeBeginScreenSession(w, h)

        val enc = buildEncoder(w, h)
        val surface = enc.createInputSurface()
        enc.start()
        codec = enc
        inputSurface = surface

        virtualDisplay = proj.createVirtualDisplay(
            "streambeam-screen",
            w,
            h,
            captureDpi,
            DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
            surface,
            null,
            null,
        )

        running = true
        drainThread = Thread({ drainLoop() }, "sbm-encoder").apply { start() }
        watchForRotation()
    }

    private fun buildEncoder(w: Int, h: Int): MediaCodec {
        val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, w, h).apply {
            setInteger(
                MediaFormat.KEY_COLOR_FORMAT,
                MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface,
            )
            // Text and UI need more detail than the old fixed 1280px capture.
            // Scale the budget with actual pixels, rather than spending the
            // same bitrate on a small phone and a high-resolution tablet.
            setInteger(MediaFormat.KEY_BIT_RATE,
                (w.toLong() * h * FPS / 8).coerceIn(8_000_000L, 24_000_000L).toInt())
            setInteger(MediaFormat.KEY_FRAME_RATE, FPS)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                // KEY_FRAME_RATE describes the intended cadence; Surface
                // producers can still submit at the display's 60/120Hz rate.
                // Drop excess input BEFORE encoding, preserving references.
                setFloat(MediaFormat.KEY_MAX_FPS_TO_ENCODER, FPS.toFloat())
                setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0)
            }
            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 2)
            setInteger(
                MediaFormat.KEY_BITRATE_MODE,
                MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR,
            )
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                setInteger(MediaFormat.KEY_LATENCY, 1)
            }
            // SPS/PPS in-band on every keyframe. This is what makes device
            // rotation work without a protocol message: after the encoder is
            // rebuilt at the rotated size, the next keyframe carries the new
            // SPS and clients reconfigure their decoder from it.
            setInteger(MediaFormat.KEY_PREPEND_HEADER_TO_SYNC_FRAMES, 1)
        }
        val enc = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
        enc.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        return enc
    }

    /** Rebuild the encoder + virtual display when the screen rotates. */
    private fun watchForRotation() {
        val dm = getSystemService(Context.DISPLAY_SERVICE) as DisplayManager
        val listener = object : DisplayManager.DisplayListener {
            override fun onDisplayAdded(displayId: Int) {}
            override fun onDisplayRemoved(displayId: Int) {}
            override fun onDisplayChanged(displayId: Int) {
                approvalHandler.post { maybeRestartForRotation() }
            }
        }
        dm.registerDisplayListener(listener, approvalHandler)
        displayListener = listener
    }

    private fun maybeRestartForRotation() {
        if (!running) return
        val metrics = DisplayMetrics()
        val wm = getSystemService(Context.WINDOW_SERVICE) as WindowManager
        @Suppress("DEPRECATION")
        wm.defaultDisplay.getRealMetrics(metrics)
        val (w, h) = scaledSize(metrics.widthPixels, metrics.heightPixels)
        if (w == captureWidth && h == captureHeight) return
        android.util.Log.i(TAG, "display rotated: ${captureWidth}x$captureHeight -> ${w}x$h")

        // Stop the drain thread before touching the codec it reads from.
        running = false
        drainThread?.join(500)
        drainThread = null
        try {
            codec?.stop()
        } catch (_: Exception) {
        }
        codec?.release()
        codec = null
        inputSurface?.release()
        inputSurface = null

        captureWidth = w
        captureHeight = h
        try {
            val enc = buildEncoder(w, h)
            val surface = enc.createInputSurface()
            enc.start()
            codec = enc
            inputSurface = surface
            // Keep the projection + virtual display; just retarget them.
            virtualDisplay?.resize(w, h, captureDpi)
            virtualDisplay?.surface = surface
        } catch (e: Exception) {
            android.util.Log.e(TAG, "encoder rebuild after rotation failed", e)
            stopSelf()
            return
        }
        running = true
        drainThread = Thread({ drainLoop() }, "sbm-encoder").apply { start() }
    }

    private fun drainLoop() {
        val enc = codec ?: return
        val info = MediaCodec.BufferInfo()
        while (running) {
            if (NativeStream.nativeTakeIdrRequest()) {
                try {
                    enc.setParameters(
                        Bundle().apply {
                            putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0)
                        },
                    )
                } catch (_: Exception) {
                }
            }
            val index = try {
                enc.dequeueOutputBuffer(info, 10_000)
            } catch (e: Exception) {
                break
            }
            if (index >= 0) {
                val buf = enc.getOutputBuffer(index)
                if (buf != null && info.size > 0) {
                    buf.position(info.offset)
                    buf.limit(info.offset + info.size)
                    val bytes = ByteArray(info.size)
                    buf.get(bytes)
                    val isConfig = (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0
                    val isKey = (info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME) != 0
                    NativeStream.nativeSubmitScreenFrame(
                        bytes, isKey, isConfig, captureWidth, captureHeight,
                    )
                }
                try {
                    enc.releaseOutputBuffer(index, false)
                } catch (_: Exception) {
                    break
                }
            }
        }
    }

    /** Poll the Rust host for incoming access requests and raise a notification. */
    private fun startApprovalPolling() {
        val poll = object : Runnable {
            override fun run() {
                if (!running) return
                val pending = try {
                    NativeStream.nativePollApprovalRequest()
                } catch (_: Exception) {
                    null
                }
                if (pending != null) {
                    val sep = pending.indexOf('|')
                    val id = pending.substring(0, if (sep >= 0) sep else pending.length)
                        .toLongOrNull() ?: -1
                    val name = if (sep >= 0) pending.substring(sep + 1) else "A device"
                    if (id >= 0 && id != shownApprovalId) {
                        shownApprovalId = id
                        showApprovalNotification(id, name)
                    }
                } else if (shownApprovalId != -1L) {
                    notificationManager().cancel(APPROVAL_NOTIF_ID)
                    shownApprovalId = -1
                }
                approvalHandler.postDelayed(this, 800)
            }
        }
        approvalPoll = poll
        approvalHandler.post(poll)
    }

    override fun onDestroy() {
        captureActive = false
        remoteCaptureActive = false
        running = false
        displayListener?.let {
            (getSystemService(Context.DISPLAY_SERVICE) as DisplayManager)
                .unregisterDisplayListener(it)
        }
        displayListener = null
        approvalPoll?.let { approvalHandler.removeCallbacks(it) }
        notificationManager().cancel(APPROVAL_NOTIF_ID)
        drainThread?.join(500)
        drainThread = null
        try {
            codec?.stop()
        } catch (_: Exception) {
        }
        codec?.release()
        codec = null
        virtualDisplay?.release()
        virtualDisplay = null
        inputSurface?.release()
        inputSurface = null
        projection?.stop()
        projection = null
        NativeStream.nativeEndScreenSession()
        super.onDestroy()
    }

    private fun notificationManager() =
        getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

    private fun ensureChannels() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val nm = notificationManager()
        nm.createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                "Screen streaming",
                NotificationManager.IMPORTANCE_LOW,
            ),
        )
        nm.createNotificationChannel(
            NotificationChannel(
                APPROVAL_CHANNEL_ID,
                "Access requests",
                NotificationManager.IMPORTANCE_HIGH,
            ),
        )
    }

    private fun startAsForeground() {
        ensureChannels()
        val stopIntent = PendingIntent.getService(
            this,
            1,
            Intent(this, ScreenCaptureService::class.java).setAction(ACTION_STOP),
            pendingFlags(),
        )
        val notification: Notification = Notification.Builder(this, CHANNEL_ID)
            .setContentTitle("StreamBeam is sharing your screen")
            .setContentText("Your screen is visible to paired devices.")
            .setSmallIcon(android.R.drawable.ic_menu_share)
            .setOngoing(true)
            .setContentIntent(openAppIntent())
            .addAction(android.R.drawable.ic_menu_close_clear_cancel, "Stop", stopIntent)
            .build()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(
                NOTIF_ID,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION,
            )
        } else {
            startForeground(NOTIF_ID, notification)
        }
    }

    private fun showApprovalNotification(id: Long, clientName: String) {
        val allow = PendingIntent.getService(
            this,
            (id * 2).toInt(),
            Intent(this, ScreenCaptureService::class.java)
                .setAction(ACTION_ALLOW)
                .putExtra(EXTRA_APPROVAL_ID, id),
            pendingFlags(),
        )
        val deny = PendingIntent.getService(
            this,
            (id * 2 + 1).toInt(),
            Intent(this, ScreenCaptureService::class.java)
                .setAction(ACTION_DENY)
                .putExtra(EXTRA_APPROVAL_ID, id),
            pendingFlags(),
        )
        val notification = Notification.Builder(this, APPROVAL_CHANNEL_ID)
            .setContentTitle("Allow this device to watch?")
            .setContentText("$clientName wants to view your screen.")
            .setSmallIcon(android.R.drawable.ic_menu_view)
            .setContentIntent(openAppIntent())
            .setAutoCancel(true)
            .addAction(android.R.drawable.ic_menu_view, "Allow", allow)
            .addAction(android.R.drawable.ic_menu_close_clear_cancel, "Deny", deny)
            .build()
        notificationManager().notify(APPROVAL_NOTIF_ID, notification)
    }

    private fun openAppIntent(): PendingIntent = PendingIntent.getActivity(
        this,
        0,
        Intent(this, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
        pendingFlags(),
    )

    private fun pendingFlags(): Int {
        var flags = PendingIntent.FLAG_UPDATE_CURRENT
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            flags = flags or PendingIntent.FLAG_IMMUTABLE
        }
        return flags
    }

    /** Scale so the longest side is at most [MAX_DIM], both dims even (H.264). */
    private fun scaledSize(width: Int, height: Int): Pair<Int, Int> {
        val longest = maxOf(width, height)
        val scale = if (longest > MAX_DIM) MAX_DIM.toDouble() / longest else 1.0
        var w = (width * scale).toInt()
        var h = (height * scale).toInt()
        w -= w % 2
        h -= h % 2
        return Pair(maxOf(w, 2), maxOf(h, 2))
    }

    companion object {
        const val TAG = "ScreenCaptureService"
        const val EXTRA_RESULT_CODE = "resultCode"
        const val EXTRA_RESULT_DATA = "resultData"
        const val EXTRA_REMOTE_REQUEST = "remoteRequest"
        const val EXTRA_APPROVAL_ID = "approvalId"
        const val ACTION_STOP = "io.streambeam.streambeam.STOP"
        const val ACTION_ALLOW = "io.streambeam.streambeam.ALLOW"
        const val ACTION_DENY = "io.streambeam.streambeam.DENY"
        private const val CHANNEL_ID = "streambeam_capture"
        private const val APPROVAL_CHANNEL_ID = "streambeam_access"
        private const val NOTIF_ID = 42
        private const val APPROVAL_NOTIF_ID = 43
        private const val MAX_DIM = 2560
        private const val FPS = 30

        @Volatile
        private var captureActive = false

        @Volatile
        private var remoteCaptureActive = false

        fun isCaptureActive(): Boolean = captureActive

        fun isRemoteCaptureActive(): Boolean = captureActive && remoteCaptureActive
    }
}

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

    @Volatile
    private var running = false

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
        val dpi = metrics.densityDpi

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

        val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, w, h).apply {
            setInteger(
                MediaFormat.KEY_COLOR_FORMAT,
                MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface,
            )
            setInteger(MediaFormat.KEY_BIT_RATE, BITRATE)
            setInteger(MediaFormat.KEY_FRAME_RATE, FPS)
            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 2)
            setInteger(
                MediaFormat.KEY_BITRATE_MODE,
                MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR,
            )
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                setInteger(MediaFormat.KEY_LATENCY, 1)
            }
        }

        val enc = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
        enc.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        val surface = enc.createInputSurface()
        enc.start()
        codec = enc
        inputSurface = surface

        virtualDisplay = proj.createVirtualDisplay(
            "streambeam-screen",
            w,
            h,
            dpi,
            DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
            surface,
            null,
            null,
        )

        running = true
        drainThread = Thread({ drainLoop(w, h) }, "sbm-encoder").apply { start() }
    }

    private fun drainLoop(w: Int, h: Int) {
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
                    NativeStream.nativeSubmitScreenFrame(bytes, isKey, isConfig, w, h)
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
        running = false
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
        const val EXTRA_APPROVAL_ID = "approvalId"
        const val ACTION_STOP = "io.streambeam.streambeam.STOP"
        const val ACTION_ALLOW = "io.streambeam.streambeam.ALLOW"
        const val ACTION_DENY = "io.streambeam.streambeam.DENY"
        private const val CHANNEL_ID = "streambeam_capture"
        private const val APPROVAL_CHANNEL_ID = "streambeam_access"
        private const val NOTIF_ID = 42
        private const val APPROVAL_NOTIF_ID = 43
        private const val MAX_DIM = 1280
        private const val FPS = 30
        private const val BITRATE = 8_000_000
    }
}

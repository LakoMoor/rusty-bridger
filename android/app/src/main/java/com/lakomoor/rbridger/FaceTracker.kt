package com.lakomoor.rbridger

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Matrix
import android.os.Handler
import android.os.Looper
import androidx.camera.core.ImageProxy
import com.google.mediapipe.framework.image.BitmapImageBuilder
import com.google.mediapipe.tasks.core.BaseOptions
import com.google.mediapipe.tasks.vision.core.RunningMode
import com.google.mediapipe.tasks.vision.facelandmarker.FaceLandmarker
import com.google.mediapipe.tasks.vision.facelandmarker.FaceLandmarkerResult
import java.io.File

data class FaceData(
    val blendShapes: List<Pair<String, Float>>,
    val pitch: Float,
    val yaw: Float,
    val roll: Float,
)

class FaceTracker(
    private val context: Context,
    private val onResult: (FaceData?) -> Unit,
) {
    private var landmarker: FaceLandmarker? = null
    private val mainHandler = Handler(Looper.getMainLooper())

    fun initialize(modelFile: File) {
        // FaceLandmarkerOptions is a nested class inside FaceLandmarker
        val options = FaceLandmarker.FaceLandmarkerOptions.builder()
            .setBaseOptions(
                BaseOptions.builder()
                    .setModelAssetPath(modelFile.absolutePath)
                    .build()
            )
            .setRunningMode(RunningMode.LIVE_STREAM)
            .setNumFaces(1)
            .setOutputFaceBlendshapes(true)
            .setOutputFacialTransformationMatrixes(true)
            .setResultListener { result: FaceLandmarkerResult, _ -> dispatchResult(result) }
            .setErrorListener { err -> err.printStackTrace() }
            .build()

        landmarker = FaceLandmarker.createFromOptions(context, options)
    }

    fun process(imageProxy: ImageProxy) {
        val bmp = imageProxy.toBitmap()
        val rotated = rotateBitmap(bmp, imageProxy.imageInfo.rotationDegrees.toFloat(), mirrorH = true)
        imageProxy.close()
        val mp = BitmapImageBuilder(rotated).build()
        landmarker?.detectAsync(mp, System.currentTimeMillis())
    }

    private fun dispatchResult(result: FaceLandmarkerResult) {
        val data = buildFaceData(result)
        mainHandler.post { onResult(data) }
    }

    private fun buildFaceData(result: FaceLandmarkerResult): FaceData? {
        if (result.faceLandmarks().isEmpty()) return null

        // faceBlendshapes() returns Optional<List<List<Category>>>
        val shapes = result.faceBlendshapes()
            .orElse(null)
            ?.firstOrNull()
            ?.filter { it.categoryName() != "_neutral" }
            ?.map { it.categoryName() to it.score() }
            ?: emptyList()

        var pitch = 0f; var yaw = 0f; var roll = 0f
        // facialTransformationMatrixes() returns Optional<List<float[]>> (row-major 4x4)
        result.facialTransformationMatrixes().orElse(null)?.firstOrNull()?.let { m ->
            pitch = Math.toDegrees(Math.asin(-m[6].toDouble())).toFloat()
            yaw   = Math.toDegrees(Math.atan2(m[4].toDouble(), m[0].toDouble())).toFloat()
            roll  = Math.toDegrees(Math.atan2(m[9].toDouble(), m[10].toDouble())).toFloat()
        }

        return FaceData(shapes, pitch, yaw, roll)
    }

    private fun rotateBitmap(src: Bitmap, degrees: Float, mirrorH: Boolean): Bitmap {
        val m = Matrix()
        if (degrees != 0f) m.postRotate(degrees)
        if (mirrorH) m.postScale(-1f, 1f, src.width / 2f, src.height / 2f)
        return if (m.isIdentity) src
        else Bitmap.createBitmap(src, 0, 0, src.width, src.height, m, true)
    }

    fun close() {
        landmarker?.close()
        landmarker = null
    }
}

package space.dualizm.duoray

import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.ComponentName
import android.content.Context
import android.os.Handler
import android.os.Looper
import android.util.Log
import java.util.concurrent.atomic.AtomicInteger

class SubscriptionJob : JobService() {
    private var running: Int? = null

    override fun onStartJob(params: JobParameters): Boolean {
        val token = generation.incrementAndGet()
        running = token
        Native.subscriptionJobToken(token)
        Thread {
            val success = runCatching {
                Native.boot(this)
                Native.refreshSubscriptions(token)
            }.onFailure { Log.w("DUORAY", "Subscription job failed", it) }.getOrDefault(false)
            Handler(Looper.getMainLooper()).post {
                if (running == token) {
                    running = null
                    jobFinished(params, !success)
                }
            }
        }.start()
        return true
    }

    override fun onStopJob(params: JobParameters): Boolean {
        running = null
        Native.subscriptionJobToken(generation.incrementAndGet())
        return true
    }

    companion object {
        private const val ID = 5173
        private val generation = AtomicInteger()
        fun schedule(context: Context) {
            val scheduler = context.getSystemService(JobScheduler::class.java)
            if (scheduler.getPendingJob(ID) != null) return
            val job = JobInfo.Builder(ID, ComponentName(context, SubscriptionJob::class.java))
                .setRequiredNetworkType(JobInfo.NETWORK_TYPE_ANY)
                .setPeriodic(15 * 60_000L)
                .setPersisted(true)
                .build()
            scheduler.schedule(job)
        }
    }
}

#!/usr/bin/env groovy

// Searches previous builds to find first non aborted one
def getLastNonAbortedBuild(build) {
  if (build == null) {
    return null;
  }

  if(build.result.toString().equals("ABORTED")) {
    return getLastNonAbortedBuild(build.getPreviousBuild());
  } else {
    return build;
  }
}

// Send out a slack message if branch got broken or has recovered
def notifySlackUponStateChange(build) {
  def cur = build.getResult()
  def prev = getLastNonAbortedBuild(build.getPreviousBuild())?.getResult()
  if (cur != prev) {
    if (cur == 'SUCCESS') {
      slackSend(
        channel: '#mayastor-backend',
        color: 'normal',
        message: "Branch ${env.BRANCH_NAME} has been fixed :beers: (<${env.BUILD_URL}|Open>)"
      )
    } else if (prev == 'SUCCESS') {
      slackSend(
        channel: '#mayastor-backend',
        color: 'danger',
        message: "Branch ${env.BRANCH_NAME} is broken :face_with_raised_eyebrow: (<${env.BUILD_URL}|Open>)"
      )
    }
  }
}

// Will ABORT current job for cases when we don't want to build
if (currentBuild.getBuildCauses('jenkins.branch.BranchIndexingCause') &&
    BRANCH_NAME == "develop") {
    print "INFO: Branch Indexing, aborting job."
    currentBuild.result = 'ABORTED'
    return
}

// Only schedule regular builds on develop branch, so we don't need to guard against it
String cron_schedule = BRANCH_NAME == "develop" ? "0 2 * * *" : ""
// Some long e2e tests are not suitable to be run for each PR
boolean run_extended_e2e_tests = (env.BRANCH_NAME != 'staging' && env.BRANCH_NAME != 'trying') ? true : false

pipeline {
  agent none
  options {
    timeout(time: 2, unit: 'HOURS')
  }
  triggers {
    cron(cron_schedule)
  }

  stages {
    stage('init') {
      agent { label 'nixos-mayastor' }
      steps {
        step([
          $class: 'GitHubSetCommitStatusBuilder',
          contextSource: [
            $class: 'ManuallyEnteredCommitContextSource',
            context: 'continuous-integration/jenkins/branch'
          ],
          statusMessage: [ content: 'Pipeline started' ]
        ])
      }
    }
    stage('linter') {
      agent { label 'nixos-mayastor' }
      when {
        beforeAgent true
        not {
          anyOf {
            branch 'master'
            branch 'release/*'
          }
        }
      }
      steps {
        sh 'nix-shell --run "cargo fmt --all -- --check"'
        sh 'nix-shell --run "cargo clippy --all-targets -- -D warnings"'
        sh 'nix-shell --run "./scripts/js-check.sh"'
      }
    }
    stage('test') {
      when {
        beforeAgent true
        not {
          anyOf {
            branch 'master'
            branch 'release/*'
          }
        }
      }
      parallel {
        stage('rust unit tests') {
          agent { label 'nixos-mayastor' }
          steps {
            sh 'printenv'
            sh 'nix-shell --run "./scripts/cargo-test.sh"'
          }
          post {
            always {
              // in case of abnormal termination of any nvmf test
              sh 'sudo nvme disconnect-all'
            }
          }
        }
        stage('grpc tests') {
          agent { label 'nixos-mayastor' }
          steps {
            sh 'printenv'
            sh 'nix-shell --run "./scripts/grpc-test.sh"'
          }
          post {
            always {
              junit '*-xunit-report.xml'
            }
          }
        }
        stage('moac unit tests') {
          agent { label 'nixos-mayastor' }
          steps {
            sh 'printenv'
            sh 'nix-shell --run "./scripts/moac-test.sh"'
          }
          post {
            always {
              junit 'moac-xunit-report.xml'
            }
          }
        }
      }
    }
    stage('e2e tests') {
      agent { label 'nixos-mayastor' }
      environment {
        GIT_COMMIT_SHORT = sh(
          // using printf to get rid of trailing newline
          script: "printf \$(git rev-parse --short ${GIT_COMMIT})",
          returnStdout: true
        )
      }
      steps {
        // e2e tests are the most demanding step for space on the disk so we
        // test the free space here rather than repeating the same code in all
        // stages.
        sh "./scripts/reclaim-space.sh 10"
        // Build images (REGISTRY is set in jenkin's global configuration).
        // Note: We might want to build and test dev images that have more
        // assertions instead but that complicates e2e tests a bit.
        sh "./scripts/release.sh --alias-tag ci --registry ${env.REGISTRY}"
        withCredentials([file(credentialsId: 'kubeconfig', variable: 'KUBECONFIG')]) {
          sh 'kubectl get nodes -o wide'
          script {
            def cmd = "./scripts/e2e-test.sh --device /dev/nvme1n1 --tag \"${env.GIT_COMMIT_SHORT}\" --registry \"${env.REGISTRY}\""
            if (run_extended_e2e_tests) {
              cmd = cmd + " --extended"
            }
            sh "nix-shell --run '${cmd}'"
          }
        }
      }
      // Always remove all docker images because they are usually used just once
      // and underlaying pkgs are already cached by nix so they can be easily
      // recreated.
      post {
        always {
          sh 'docker image prune --all --force'
        }
      }
    }
    stage('push images') {
      agent { label 'nixos-mayastor' }
      when {
        beforeAgent true
        anyOf {
          branch 'master'
          branch 'release/*'
          branch 'develop'
        }
      }
      steps {
        withCredentials([usernamePassword(credentialsId: 'dockerhub', usernameVariable: 'USERNAME', passwordVariable: 'PASSWORD')]) {
          sh 'echo $PASSWORD | docker login -u $USERNAME --password-stdin'
        }
        sh './scripts/release.sh'
      }
      post {
        always {
          sh 'docker logout'
          sh 'docker image prune --all --force'
        }
      }
    }
  }

  // The main motivation for post block is that if all stages were skipped
  // (which happens when running cron job and branch != develop) then we don't
  // want to set commit status in github (jenkins will implicitly set it to
  // success).
  post {
    always {
      node(null) {
        script {
          // If no tests were run then we should neither be updating commit
          // status in github nor send any slack messages
          if (currentBuild.result != null) {
            step([
              $class: 'GitHubCommitStatusSetter',
              errorHandlers: [[$class: "ChangingBuildStatusErrorHandler", result: "UNSTABLE"]],
              contextSource: [
                $class: 'ManuallyEnteredCommitContextSource',
                context: 'continuous-integration/jenkins/branch'
              ],
              statusResultSource: [
                $class: 'ConditionalStatusResultSource',
                results: [
                  [$class: 'AnyBuildResult', message: 'Pipeline result', state: currentBuild.getResult()]
                ]
              ]
            ])
            if (env.BRANCH_NAME == 'develop') {
              notifySlackUponStateChange(currentBuild)
            }
          }
        }
      }
    }
  }
}

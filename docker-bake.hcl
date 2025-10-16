variable "REGISTRY" {
  default = "localhost"
}

variable "REPO" {
  default = "wlsctx"
}

variable "DISTRO_RELEASE" {
  default = "43"
}

function "today" {
  params = []
  result = formatdate("YYYYMMDD", timestamp())
}

function "tags" {
  params = [img]
  result = distinct([
    "${img}:latest",
    notequal("",TAG) ? "${img}:${TAG}": ""
  ])
}

variable "TAG" {
  default = "${DISTRO_RELEASE}." + today()
}

group "default" {
  targets = [
    "runtime",
    "devel",
  ]
}

target "_runtime_common" {
  dockerfile = "Containerfile"
  contexts = {
    fedora = "docker-image://registry.fedoraproject.org/fedora:${DISTRO_RELEASE}"
  }
}

target "base" {
  inherits = ["_runtime_common"]
  context = "apps/base"
}

target "runtime" {
  inherits = ["_runtime_common"]
  context = "containers/apps/${tgt}"
  contexts = {
    base = "target:base"
  }
  matrix = {
    tgt = ["shell", "wayland", "java"]
  }
  name = "${tgt}-runtime"
  target = "runtime"
  tags = tags("${tgt}-runtime")
}

target "devel" {
  inherits = ["_runtime_common"]
  context = "containers/apps/${tgt}"
  contexts = {
    base = "target:${tgt}-runtime"
  }
  matrix = {
    tgt = ["shell", "wayland"]
  }
  name = "${tgt}-devel"
  target = "devel"
  tags = tags("${tgt}-devel")
}

target "_app_common" {

}

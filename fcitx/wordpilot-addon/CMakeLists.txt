# Thin Fcitx 5 adapter for WordPilot (spec §19-§21, §60).
#
# This is the ONLY piece of the project written in C++, and it contains no
# business logic: no ranking, no dictionaries, no AI. It translates Fcitx key
# events into requests on the WordPilot daemon's Unix socket and translates
# the daemon's JSON responses into a Fcitx candidate list. Everything else
# lives in the Rust workspace one level up.
cmake_minimum_required(VERSION 3.16)
project(wordpilot-fcitx5 VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

find_package(Fcitx5Core REQUIRED)
find_package(Fcitx5Utils REQUIRED)

include(FeatureSummary)
include(GNUInstallDirs)

add_fcitx5_addon(wordpilot
    src/main.cpp
    src/engine.cpp
    src/socketclient.cpp
    src/json.cpp
)
target_include_directories(wordpilot PRIVATE src)
target_link_libraries(wordpilot Fcitx5::Core Fcitx5::Utils)

install(TARGETS wordpilot DESTINATION "${CMAKE_INSTALL_LIBDIR}/fcitx5")
install(FILES wordpilot-addon.conf DESTINATION "${CMAKE_INSTALL_DATADIR}/fcitx5/addon")

feature_summary(WHAT ALL FATAL_ON_MISSING_REQUIRED_PACKAGES)

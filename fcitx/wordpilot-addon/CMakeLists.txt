# Thin Fcitx 5 adapter for WordPilot.
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

# Fcitx only loads addons from its own directories, so install next to Fcitx
# unless a prefix was given explicitly. Fcitx's CMake config then supplies the
# exact paths (e.g. /usr/lib/x86_64-linux-gnu/fcitx5 on Debian).
if (CMAKE_INSTALL_PREFIX_INITIALIZED_TO_DEFAULT)
    set(CMAKE_INSTALL_PREFIX "/usr" CACHE PATH "Install prefix" FORCE)
endif()
if (NOT CMAKE_BUILD_TYPE AND NOT CMAKE_CONFIGURATION_TYPES)
    set(CMAKE_BUILD_TYPE Release CACHE STRING "Build type" FORCE)
endif()

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

if (FCITX_INSTALL_USE_FCITX_SYS_PATHS)
    set(WORDPILOT_ADDON_DIR "${FCITX_INSTALL_ADDONDIR}")
    set(WORDPILOT_ADDON_CONF_DIR "${FCITX_INSTALL_PKGDATADIR}/addon")
else()
    set(WORDPILOT_ADDON_DIR "${CMAKE_INSTALL_LIBDIR}/fcitx5")
    set(WORDPILOT_ADDON_CONF_DIR "${CMAKE_INSTALL_DATADIR}/fcitx5/addon")
endif()
install(TARGETS wordpilot DESTINATION "${WORDPILOT_ADDON_DIR}")
install(FILES wordpilot-addon.conf DESTINATION "${WORDPILOT_ADDON_CONF_DIR}")

# Integration test: drives the built addon inside an in-process Fcitx
# instance through Fcitx's own TestFrontend, against a fake daemon. Needs the
# test modules the `fcitx5` package ships (libtestfrontend.so & co).
option(ENABLE_TEST "Build the addon integration test" ON)
if (ENABLE_TEST)
    find_package(Fcitx5ModuleTestFrontend QUIET)
    if (TARGET Fcitx5::Module::TestFrontend)
        enable_testing()
        # Fcitx looks for addon .conf files under <data dir>/addon.
        configure_file(wordpilot-addon.conf
            "${CMAKE_CURRENT_BINARY_DIR}/test/addon/wordpilot-addon.conf" COPYONLY)
        add_executable(testwordpilot test/testwordpilot.cpp src/json.cpp)
        target_include_directories(testwordpilot PRIVATE src)
        target_link_libraries(testwordpilot
            Fcitx5::Core Fcitx5::Utils Fcitx5::Module::TestFrontend)
        target_compile_definitions(testwordpilot PRIVATE
            WORDPILOT_TEST_BINARY_DIR="${CMAKE_CURRENT_BINARY_DIR}")
        add_dependencies(testwordpilot wordpilot)
        add_test(NAME testwordpilot COMMAND testwordpilot)
    else()
        message(STATUS "Fcitx5ModuleTestFrontend not found; skipping the integration test")
    endif()
endif()

feature_summary(WHAT ALL FATAL_ON_MISSING_REQUIRED_PACKAGES)
